//! What a frame is written with: the boolean entropy encoder of RFC 6386
//! §7.3, the inverse of [`Bool`](super::Bool), and the syntax written over it
//! — trees, tokens, vectors and headers — each the mirror of the reader the
//! decoder keeps.
//!
//! A byte the encoder finds no room for is recorded, and the partition is
//! refused when it is closed, as the reader records running out and is
//! checked once its frame is read: no decision is coded against a fallible
//! return.
//!
//! Trees, tokens and vectors are written to a [`Sink`], which the encoder
//! codes and an estimate sums the cost of, so a choice is estimated by the
//! code that writes it.

use alloc::vec::Vec;

use tairix_util::fallible;

use super::{
    Buffer, BufferUpdate, Header, Probabilities, Segmentation, StreamState, BANDS, BLOCK_COEFFS,
    CATEGORY_BASES, CATEGORY_PROBS, COEFF_BANDS, COEFF_TREE, COEFF_UPDATE_PROBS, CONTEXTS,
    DCT_CAT1, DCT_EOB, MV_IS_SHORT, MV_LONG, MV_LONG_BITS, MV_PROBS, MV_REF_TREE, MV_SHORT,
    MV_SIGN, MV_SPLIT, MV_UPDATE_PROBS, NODES, SMALL_MV_TREE, TREE_AFTER_EOB,
};
#[cfg(any(test, feature = "encode"))]
use super::{
    BMODE_TREE, B_HU_PRED, B_PRED, DEFAULT_COEFF_PROBS, KF_YMODE_TREE, PLANES, TM_PRED,
    UV_MODE_TREE, YMODE_TREE,
};

/// What a coded decision goes to.
pub(super) trait Sink {
    /// The decision `value`, with the probability `probability` of its being
    /// false.
    fn bit(&mut self, probability: u8, value: bool);
}

/// The boolean entropy encoder from the format's own specification.
pub(super) struct Writer {
    out: Vec<u8>,
    /// Where the partition begins in `out`: no carry runs further back.
    start: usize,
    /// The most bytes `out` may hold.
    room: usize,
    range: u32,
    bottom: u32,
    count: i32,
    /// Whether a byte found no room, which refuses the partition.
    refused: bool,
}

impl Writer {
    /// A writer of a partition of its own.
    #[cfg(any(test, feature = "fixture"))]
    pub(super) fn new() -> Self {
        Self::onto(Vec::new())
    }

    /// A writer appending a partition to what `out` holds.
    pub(super) fn onto(out: Vec<u8>) -> Self {
        Self {
            start: out.len(),
            room: usize::MAX,
            out,
            range: 255,
            bottom: 0,
            count: 24,
            refused: false,
        }
    }

    /// A writer appending a partition to what `out` holds, refusing it rather
    /// than growing `out` past the room it already has.
    #[cfg(feature = "encode")]
    pub(super) fn within(out: Vec<u8>) -> Self {
        Self {
            room: out.capacity(),
            ..Self::onto(out)
        }
    }

    /// Code `value` with the probability `probability` of its being false.
    pub(super) fn bit(&mut self, probability: u8, value: bool) {
        let split = 1 + (((self.range - 1) * u32::from(probability)) >> 8);
        if value {
            self.bottom = self.bottom.wrapping_add(split);
            self.range -= split;
        } else {
            self.range = split;
        }
        // Back to 128 or more in one shift, written out in at most two steps
        // around the byte that shift completes: the same bytes and carries as
        // a step at a time.
        let shift = self.range.leading_zeros().saturating_sub(24);
        self.range <<= shift;
        let before = shift.min(self.count.unsigned_abs());
        self.shift_out(before);
        if self.count == 0 {
            self.emit(self.bottom.to_be_bytes()[0]);
            self.bottom &= (1 << 24) - 1;
            self.count = 8;
            self.shift_out(shift - before);
        }
    }

    /// Shift `bottom` left `bits` places, fewer than eight, carrying each bit
    /// that leaves its top into the bytes written.
    fn shift_out(&mut self, bits: u32) {
        if bits == 0 {
            return;
        }
        for _ in 0..(self.bottom >> (32 - bits)).count_ones() {
            self.carry();
        }
        self.bottom <<= bits;
        self.count -= i32::try_from(bits).unwrap_or(0);
    }

    /// Code an even decision.
    pub(super) fn flag(&mut self, value: bool) {
        self.bit(128, value);
    }

    /// Code an unsigned literal of `bits` bits, most significant first.
    pub(super) fn literal(&mut self, value: u32, bits: u32) {
        for index in (0..bits).rev() {
            self.flag((value >> index) & 1 != 0);
        }
    }

    /// Close the partition, writing out the bits still pending: its bytes
    /// after what `out` held, or, where a byte found no room, `out` as it was
    /// before the partition began.
    pub(super) fn close(mut self) -> Result<Vec<u8>, Vec<u8>> {
        let count = self.count;
        let mut value = self.bottom;
        let carried = 1u32.wrapping_shl(u32::try_from(32 - count).unwrap_or(0));
        if count < 32 && (value & carried) != 0 {
            self.carry();
        }
        value = value.wrapping_shl(u32::try_from(count & 7).unwrap_or(0));
        for _ in 0..(count >> 3) {
            value = value.wrapping_shl(8);
        }
        for _ in 0..4 {
            self.emit(value.to_be_bytes()[0]);
            value = value.wrapping_shl(8);
        }
        if self.refused {
            self.out.truncate(self.start);
            Err(self.out)
        } else {
            Ok(self.out)
        }
    }

    /// Append `byte`, or record that it found no room.
    fn emit(&mut self, byte: u8) {
        if self.out.len() >= self.room || !fallible::push(&mut self.out, byte) {
            self.refused = true;
        }
    }

    /// Carry one into the partition's bytes already written.
    fn carry(&mut self) {
        let written = self.out.get_mut(self.start..).unwrap_or_default();
        for byte in written.iter_mut().rev() {
            if *byte == u8::MAX {
                *byte = 0;
            } else {
                *byte += 1;
                return;
            }
        }
    }
}

impl Sink for Writer {
    fn bit(&mut self, probability: u8, value: bool) {
        Self::bit(self, probability, value);
    }
}

/// What coding the decisions it is given costs, in 256ths of a bit.
#[cfg(any(test, feature = "encode"))]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct Cost(pub(super) u32);

#[cfg(any(test, feature = "encode"))]
impl Sink for Cost {
    fn bit(&mut self, probability: u8, value: bool) {
        self.0 = self.0.saturating_add(cost(probability, value));
    }
}

/// What coding `value` with the probability `probability` of its being false
/// costs, in 256ths of a bit.
#[cfg(any(test, feature = "encode"))]
pub(super) const fn cost(probability: u8, value: bool) -> u32 {
    let odds = if value {
        256 - probability as usize
    } else {
        probability as usize
    };
    BIT_COSTS[odds]
}

/// What coding a decision whose odds are `n` in 256 costs, by `n`: eight bits
/// less the odds' logarithm, in 256ths of a bit. Odds of nought cost as one,
/// the least a split leaves.
#[cfg(any(test, feature = "encode"))]
const BIT_COSTS: [u32; 257] = bit_costs();

#[cfg(any(test, feature = "encode"))]
const fn bit_costs() -> [u32; 257] {
    let mut costs = [0u32; 257];
    let mut odds: u32 = 1;
    while odds <= 256 {
        costs[odds as usize] = 8 * 256 - log2_256ths(odds);
        odds += 1;
    }
    costs[0] = costs[1];
    costs
}

/// `log₂ n` in 256ths, rounded down, for `n` of one or more: its highest set
/// bit, then each bit of the fraction found by squaring what is left.
#[cfg(any(test, feature = "encode"))]
const fn log2_256ths(n: u32) -> u32 {
    let whole = n.ilog2();
    let mut mantissa = (n as u64) << (16 - whole);
    let mut fraction = 0;
    let mut bit = 0;
    while bit < 8 {
        mantissa = (mantissa * mantissa) >> 16;
        fraction <<= 1;
        if mantissa >= 2 << 16 {
            mantissa >>= 1;
            fraction |= 1;
        }
        bit += 1;
    }
    (whole << 8) | fraction
}

/// The deepest any of the format's trees goes: half its largest's entries.
const MAX_DEPTH: usize = COEFF_TREE.len() / 2;

/// The branches from a tree's root to one of its leaves, each a node — the
/// index of its pair of entries — and the branch taken from it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct Path {
    steps: [(usize, bool); MAX_DEPTH],
    len: usize,
}

impl Path {
    /// No branches at all.
    const EMPTY: Self = Self {
        steps: [(0, false); MAX_DEPTH],
        len: 0,
    };

    /// The path through `tree` from node `start` to the leaf `value`, or none
    /// where the tree has no such leaf below `start`.
    pub(super) const fn to(tree: &[i8], start: usize, value: u8) -> Self {
        let mut at = 0;
        while at < tree.len() && !names_leaf(tree[at], value) {
            at += 1;
        }
        let mut climbed = Self::EMPTY;
        while at < tree.len() && climbed.len < MAX_DEPTH {
            let node = at - at % 2;
            climbed.steps[climbed.len] = (node, at % 2 == 1);
            climbed.len += 1;
            if node == start {
                return climbed.reversed();
            }
            let mut parent = 0;
            while parent < tree.len() && !names_node(tree[parent], node) {
                parent += 1;
            }
            at = parent;
        }
        Self::EMPTY
    }

    /// Every leaf's path from the root of `tree`, the leaves numbered from
    /// nought.
    pub(super) const fn all<const LEAVES: usize>(tree: &[i8]) -> [Self; LEAVES] {
        let mut paths = [Self::EMPTY; LEAVES];
        let mut leaf: u8 = 0;
        while (leaf as usize) < LEAVES {
            paths[leaf as usize] = Self::to(tree, 0, leaf);
            leaf += 1;
        }
        paths
    }

    /// What coding this path costs with `probs`, in 256ths of a bit.
    #[cfg(any(test, feature = "encode"))]
    pub(super) const fn cost(&self, probs: &[u8]) -> u32 {
        let mut total = 0;
        let mut step = 0;
        while step < self.len {
            let (node, branch) = self.steps[step];
            let probability = if node / 2 < probs.len() {
                probs[node / 2]
            } else {
                128
            };
            total += cost(probability, branch);
            step += 1;
        }
        total
    }

    /// These steps the other way round.
    const fn reversed(self) -> Self {
        let mut path = Self::EMPTY;
        while path.len < self.len {
            path.steps[path.len] = self.steps[self.len - 1 - path.len];
            path.len += 1;
        }
        path
    }

    /// The steps, root first.
    fn steps(&self) -> &[(usize, bool)] {
        self.steps.get(..self.len).unwrap_or_default()
    }
}

/// Whether a tree's `entry` is the leaf `value`: a leaf is its value negated.
const fn names_leaf(entry: i8, value: u8) -> bool {
    entry <= 0 && entry.unsigned_abs() == value
}

/// Whether a tree's `entry` leads on to `node`.
const fn names_node(entry: i8, node: usize) -> bool {
    entry > 0 && entry.unsigned_abs() as usize == node
}

/// Each token's path from the root of the coefficient tree: the eleven
/// values and the end of a block.
static TOKEN_PATHS: [Path; TOKENS] = Path::all(&COEFF_TREE);

/// The coefficient tree's tokens: the eleven values and the end of a block.
const TOKENS: usize = DCT_EOB as usize + 1;

/// Each mode's path through the tree an inter macroblock's mode is read
/// with, by the code it names.
pub(super) const MV_REF_PATHS: [Path; MV_SPLIT as usize + 1] = Path::all(&MV_REF_TREE);

/// Each whole-macroblock luma mode's path through a keyframe's tree.
#[cfg(any(test, feature = "encode"))]
pub(super) const KF_YMODE_PATHS: [Path; B_PRED as usize + 1] = Path::all(&KF_YMODE_TREE);

/// Each one's path through an inter frame's.
#[cfg(any(test, feature = "encode"))]
pub(super) const YMODE_PATHS: [Path; B_PRED as usize + 1] = Path::all(&YMODE_TREE);

/// Each chroma mode's path.
#[cfg(any(test, feature = "encode"))]
pub(super) const UV_MODE_PATHS: [Path; TM_PRED as usize + 1] = Path::all(&UV_MODE_TREE);

/// Each subblock mode's path.
#[cfg(any(test, feature = "encode"))]
pub(super) const BMODE_PATHS: [Path; B_HU_PRED as usize + 1] = Path::all(&BMODE_TREE);

/// Write `path` through a tree whose nodes `probs` weighs.
pub(super) fn write_path(sink: &mut impl Sink, path: &Path, probs: &[u8]) {
    for &(node, branch) in path.steps() {
        sink.bit(probs.get(node >> 1).copied().unwrap_or(128), branch);
    }
}

/// Write the path through `tree` that reads back as `value`.
pub(super) fn write_tree(sink: &mut impl Sink, tree: &[i8], probs: &[u8], value: usize) {
    write_tree_from(sink, tree, probs, 0, value);
}

/// Write the path through `tree` from node `start` that reads back as
/// `value`; nothing where the tree has no such leaf below `start`.
pub(super) fn write_tree_from(
    sink: &mut impl Sink,
    tree: &[i8],
    probs: &[u8],
    start: usize,
    value: usize,
) {
    let leaf = u8::try_from(value).unwrap_or(u8::MAX);
    write_path(sink, &Path::to(tree, start, leaf), probs);
}

/// One plane's token probabilities: by band, context and node.
pub(super) type PlaneProbs = [[[u8; NODES]; CONTEXTS]; BANDS];

/// What a block's tokens go to: each token, and each bit beside them whose
/// probability no header adapts.
pub(super) trait TokenSink {
    /// Token `value` from node `start` of the coefficient tree, in band
    /// `band` under context `context`.
    fn token(&mut self, at: (usize, usize, usize), value: u8);

    /// A large coefficient's extra bit, or a sign.
    fn fixed(&mut self, probability: u8, value: bool);
}

/// The branches token `value` takes from node `start` of the coefficient
/// tree: each node and the branch taken from it.
fn branches(value: u8, start: usize) -> impl Iterator<Item = &'static (usize, bool)> {
    let steps = TOKEN_PATHS
        .get(usize::from(value))
        .map_or(&[][..], Path::steps);
    steps.iter().skip_while(move |&&(node, _)| node != start)
}

/// Tokens coded into a [`Sink`] with one plane's probabilities.
pub(super) struct Coded<'a, S> {
    pub(super) sink: &'a mut S,
    pub(super) probs: &'a PlaneProbs,
}

impl<S: Sink> TokenSink for Coded<'_, S> {
    fn token(&mut self, (band, context, start): (usize, usize, usize), value: u8) {
        let probs = self.probs.get(band).and_then(|band| band.get(context));
        for &(node, branch) in branches(value, start) {
            let probability = probs.and_then(|probs| probs.get(node >> 1));
            self.sink.bit(probability.copied().unwrap_or(128), branch);
        }
    }

    fn fixed(&mut self, probability: u8, value: bool) {
        self.sink.bit(probability, value);
    }
}

/// What each token costs, from the tree's root and after a zero, by band and
/// context, under one plane's probabilities: in 256ths of a bit.
#[cfg(any(test, feature = "encode"))]
pub(super) type TokenCosts = [[[[u32; TOKENS]; 2]; CONTEXTS]; BANDS];

/// The token costs of the default probabilities, by plane: what a frame's
/// first pass prices its residuals with, before its own are chosen.
#[cfg(any(test, feature = "encode"))]
pub(super) static DEFAULT_TOKEN_COSTS: [TokenCosts; PLANES] = token_costs(&DEFAULT_COEFF_PROBS);

/// Every token's cost under `probs`, summed along its path.
#[cfg(any(test, feature = "encode"))]
const fn token_costs(probs: &[PlaneProbs; PLANES]) -> [TokenCosts; PLANES] {
    let paths: [Path; TOKENS] = Path::all(&COEFF_TREE);
    let mut costs = [[[[[0; TOKENS]; 2]; CONTEXTS]; BANDS]; PLANES];
    let mut at = 0;
    while at < PLANES * BANDS * CONTEXTS * 2 * TOKENS {
        let (token, rest) = (at % TOKENS, at / TOKENS);
        let (after_zero, rest) = (rest % 2, rest / 2);
        let (context, rest) = (rest % CONTEXTS, rest / CONTEXTS);
        let (band, plane) = (rest % BANDS, rest / BANDS);
        let path = &paths[token];
        let start = if after_zero == 1 { TREE_AFTER_EOB } else { 0 };
        let mut step = 0;
        while step < path.len && path.steps[step].0 != start {
            step += 1;
        }
        let mut spent = 0;
        while step < path.len {
            let (node, branch) = path.steps[step];
            spent += cost(probs[plane][band][context][node / 2], branch);
            step += 1;
        }
        costs[plane][band][context][after_zero][token] = spent;
        at += 1;
    }
    costs
}

/// Tokens priced with one plane's [`TokenCosts`], in 256ths of a bit.
#[cfg(any(test, feature = "encode"))]
pub(super) struct Priced<'a> {
    pub(super) costs: &'a TokenCosts,
    pub(super) spent: u32,
}

#[cfg(any(test, feature = "encode"))]
impl TokenSink for Priced<'_> {
    fn token(&mut self, (band, context, start): (usize, usize, usize), value: u8) {
        let after_zero = usize::from(start != 0);
        let price = self
            .costs
            .get(band)
            .and_then(|band| band.get(context))
            .and_then(|context| context.get(after_zero))
            .and_then(|tokens| tokens.get(usize::from(value)));
        self.spent = self.spent.saturating_add(price.copied().unwrap_or(0));
    }

    fn fixed(&mut self, probability: u8, value: bool) {
        self.spent = self.spent.saturating_add(cost(probability, value));
    }
}

/// How often each branch of one plane's coefficient tree is taken: by band,
/// context and node, the false and the true.
#[cfg(any(test, feature = "encode"))]
pub(super) type PlaneCounts = [[[[u32; 2]; NODES]; CONTEXTS]; BANDS];

/// Tokens counted into one plane's counts and coded nowhere.
#[cfg(any(test, feature = "encode"))]
pub(super) struct Counted<'a>(pub(super) &'a mut PlaneCounts);

#[cfg(any(test, feature = "encode"))]
impl TokenSink for Counted<'_> {
    fn token(&mut self, (band, context, start): (usize, usize, usize), value: u8) {
        let Some(counts) = self.0.get_mut(band).and_then(|band| band.get_mut(context)) else {
            return;
        };
        for &(node, branch) in branches(value, start) {
            let count = counts
                .get_mut(node >> 1)
                .and_then(|node| node.get_mut(usize::from(branch)));
            if let Some(count) = count {
                *count = count.saturating_add(1);
            }
        }
    }

    fn fixed(&mut self, _: u8, _: bool) {}
}

/// Code one block's quantised `levels`, in scan order, from position `first`,
/// its first token under `context`: each coefficient's token, extra bits and
/// sign to the last that is not nought, then the end of the block where that
/// is not its last position. Answers whether the tokens go past `first`, as a
/// reader holds it.
pub(super) fn code_block(
    sink: &mut impl TokenSink,
    context: usize,
    first: usize,
    levels: &[i16; BLOCK_COEFFS],
) -> bool {
    let band = |scan: usize| usize::from(COEFF_BANDS.get(scan).copied().unwrap_or(0));
    let Some(last) = (first..BLOCK_COEFFS).rev().find(|&scan| levels[scan] != 0) else {
        sink.token((band(first), context, 0), DCT_EOB);
        return false;
    };
    let mut context = context;
    let mut start = 0;
    for (scan, &level) in levels.iter().enumerate().take(last + 1).skip(first) {
        let magnitude = level.unsigned_abs();
        coefficient(sink, (band(scan), context, start), magnitude);
        if magnitude != 0 {
            sink.fixed(128, level < 0);
        }
        context = match magnitude {
            0 => 0,
            1 => 1,
            _ => 2,
        };
        start = if magnitude == 0 { TREE_AFTER_EOB } else { 0 };
    }
    if last + 1 < BLOCK_COEFFS {
        sink.token((band(last + 1), context, 0), DCT_EOB);
    }
    true
}

/// Code the token of a coefficient of `magnitude`, in the band and context
/// and from the node `at` names, and the extra bits a large one carries.
fn coefficient(sink: &mut impl TokenSink, at: (usize, usize, usize), magnitude: u16) {
    let value = i32::from(magnitude);
    let Some(category) = CATEGORY_BASES.iter().rposition(|&base| value >= base) else {
        // Below the first category a token names its magnitude.
        sink.token(at, u8::try_from(magnitude).unwrap_or(DCT_CAT1 - 1));
        return;
    };
    sink.token(at, DCT_CAT1 + u8::try_from(category).unwrap_or(0));
    let bits = CATEGORY_PROBS.get(category).copied().unwrap_or_default();
    let extra = value - CATEGORY_BASES.get(category).copied().unwrap_or(0);
    for (index, &probability) in bits.iter().enumerate() {
        let shift = bits.len() - 1 - index;
        sink.fixed(probability, (extra >> shift) & 1 != 0);
    }
}

/// Write one block's quantised `levels`, in scan order, from position
/// `first`, its first token under `context`, with one plane's probabilities;
/// answers whether its tokens go past `first`.
pub(super) fn write_block(
    writer: &mut Writer,
    plane: &PlaneProbs,
    context: usize,
    first: usize,
    levels: &[i16; BLOCK_COEFFS],
) -> bool {
    let mut coded = Coded {
        sink: writer,
        probs: plane,
    };
    code_block(&mut coded, context, first, levels)
}

/// Write a vector as the bitstream codes it, its row first: `(row, column)`
/// in quarter pixels, which a reader doubles.
pub(super) fn write_mv(
    sink: &mut impl Sink,
    probs: &[[u8; MV_PROBS]; 2],
    (row, column): (i16, i16),
) {
    write_component(sink, &probs[0], row);
    write_component(sink, &probs[1], column);
}

/// Whether a component of `quarters` quarter pixels is one the bitstream
/// codes: under the long form's bits.
#[cfg(any(test, feature = "encode"))]
pub(super) fn codable(quarters: i16) -> bool {
    quarters.unsigned_abs() < 1 << MV_LONG_BITS
}

/// One component: a magnitude under eight from the short tree, a larger one
/// bit by bit with bit three left implied where no higher bit is set, then
/// the sign of any but nought. A magnitude is held to what the long form's
/// bits carry.
fn write_component(sink: &mut impl Sink, probs: &[u8; MV_PROBS], value: i16) {
    let magnitude = value.unsigned_abs().min((1 << MV_LONG_BITS) - 1);
    let long = magnitude >= 8;
    sink.bit(probs[MV_IS_SHORT], long);
    if long {
        for bit in (0..3).chain((4..MV_LONG_BITS).rev()) {
            sink.bit(probs[MV_LONG + bit], (magnitude >> bit) & 1 != 0);
        }
        if magnitude >> 4 != 0 {
            sink.bit(probs[MV_LONG + 3], (magnitude >> 3) & 1 != 0);
        }
    } else {
        let short = probs.get(MV_SHORT..MV_LONG).unwrap_or_default();
        write_tree(sink, &SMALL_MV_TREE, short, usize::from(magnitude));
    }
    if magnitude != 0 {
        sink.bit(probs[MV_SIGN], value < 0);
    }
}

/// Write the compressed header that reads, against `from`, as `header`: a
/// keyframe's against the defaults it resets to, and each value a stream
/// carries as an update only where it differs from the one carried.
pub(super) fn write_header(writer: &mut Writer, header: &Header, from: &StreamState) {
    let defaults = StreamState::new();
    let from = if header.inter.is_some() {
        from
    } else {
        // A keyframe's colour space and clamping type.
        writer.flag(false);
        writer.flag(false);
        &defaults
    };
    write_segmentation(writer, &header.segmentation, &from.segmentation);
    writer.flag(header.simple_filter);
    writer.literal(header.filter_level.unsigned_abs(), 6);
    writer.literal(header.sharpness, 3);
    let deltas = [header.deltas.reference, header.deltas.mode];
    let carried = [from.deltas.reference, from.deltas.mode];
    writer.flag(header.deltas.enabled);
    if header.deltas.enabled {
        writer.flag(deltas != carried);
        if deltas != carried {
            for (&value, &was) in deltas.as_flattened().iter().zip(carried.as_flattened()) {
                writer.flag(value != was);
                if value != was {
                    write_signed(writer, value, 6);
                }
            }
        }
    }
    writer.literal(header.partitions.trailing_zeros(), 2);
    let quant = &header.quant;
    writer.literal(quant.index.unsigned_abs(), 7);
    for delta in [
        quant.y_dc,
        quant.y2_dc,
        quant.y2_ac,
        quant.uv_dc,
        quant.uv_ac,
    ] {
        writer.flag(delta != 0);
        if delta != 0 {
            write_signed(writer, delta, 4);
        }
    }
    if let Some(inter) = &header.inter {
        let buffers = &inter.buffers;
        writer.flag(buffers.golden == BufferUpdate::Refresh);
        writer.flag(buffers.alternate == BufferUpdate::Refresh);
        write_copy(writer, buffers.golden);
        write_copy(writer, buffers.alternate);
        writer.flag(buffers.sign_bias_golden);
        writer.flag(buffers.sign_bias_alternate);
    }
    writer.flag(header.refresh_entropy);
    if let Some(inter) = &header.inter {
        writer.flag(inter.refresh_last);
    }
    write_probabilities(writer, header, &from.probs);
}

/// The segmentation fields that read, against `from`, as `segmentation`.
fn write_segmentation(writer: &mut Writer, segmentation: &Segmentation, from: &Segmentation) {
    writer.flag(segmentation.enabled);
    if !segmentation.enabled {
        return;
    }
    writer.flag(segmentation.update_map);
    let features = (
        segmentation.absolute,
        segmentation.quant,
        segmentation.filter,
    );
    let update = features != (from.absolute, from.quant, from.filter);
    writer.flag(update);
    if update {
        writer.flag(segmentation.absolute);
        for (values, bits) in [(segmentation.quant, 7), (segmentation.filter, 6)] {
            for value in values {
                writer.flag(value != 0);
                if value != 0 {
                    write_signed(writer, value, bits);
                }
            }
        }
    }
    if segmentation.update_map {
        for probability in segmentation.probs {
            writer.flag(probability != u8::MAX);
            if probability != u8::MAX {
                writer.literal(u32::from(probability), 8);
            }
        }
    }
}

/// The probability fields that read, against `from`, as `header`'s: the
/// token updates and the skip probability, then an inter frame's own. A
/// vector probability is carried as seven bits doubled, so one written is
/// read back as the even value at or below it, or as one.
fn write_probabilities(writer: &mut Writer, header: &Header, from: &Probabilities) {
    let probs = &header.probs;
    let updates = COEFF_UPDATE_PROBS
        .as_flattened()
        .as_flattened()
        .as_flattened();
    let tokens = token_probs(probs).iter().zip(token_probs(from));
    for ((&value, &was), &update) in tokens.zip(updates) {
        writer.bit(update, value != was);
        if value != was {
            writer.literal(u32::from(value), 8);
        }
    }
    writer.flag(header.skip_probability.is_some());
    if let Some(probability) = header.skip_probability {
        writer.literal(u32::from(probability), 8);
    }
    let Some(inter) = &header.inter else {
        return;
    };
    for probability in [inter.prob_intra, inter.prob_last, inter.prob_golden] {
        writer.literal(u32::from(probability), 8);
    }
    let modes = [
        (&probs.ymode[..], &from.ymode[..]),
        (&probs.uvmode[..], &from.uvmode[..]),
    ];
    for (values, carried) in modes {
        writer.flag(values != carried);
        if values != carried {
            for &value in values {
                writer.literal(u32::from(value), 8);
            }
        }
    }
    let vectors = probs.mv.as_flattened().iter().zip(from.mv.as_flattened());
    for ((&value, &was), &update) in vectors.zip(MV_UPDATE_PROBS.as_flattened()) {
        writer.bit(update, value != was);
        if value != was {
            writer.literal(u32::from(value >> 1), 7);
        }
    }
}

/// Every token probability, in the order a header updates them.
fn token_probs(probs: &Probabilities) -> &[u8] {
    probs.coeff.as_flattened().as_flattened().as_flattened()
}

/// A magnitude-then-sign literal of `bits` bits.
fn write_signed(writer: &mut Writer, value: i32, bits: u32) {
    writer.literal(value.unsigned_abs(), bits);
    writer.flag(value < 0);
}

/// The copy field of a buffer the frame does not refresh.
fn write_copy(writer: &mut Writer, update: BufferUpdate) {
    let code = match update {
        BufferUpdate::Refresh => return,
        BufferUpdate::Keep => 0,
        BufferUpdate::CopyOf(Buffer::Last) => 1,
        BufferUpdate::CopyOf(Buffer::Golden | Buffer::Alternate) => 2,
    };
    writer.literal(code, 2);
}

#[cfg(test)]
#[path = "writer_tests.rs"]
mod tests;

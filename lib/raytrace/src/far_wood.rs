//! A wood carried on far off: the trees its wood's thinning keeps across the
//! view beyond those stood one by one, traced from the cells of the lattice
//! they stand in rather than one by one, so a wood reaches as far as its
//! trees still span a pixel for no more than the walk a ray takes across it.
//!
//! The thinning marks each cell that keeps a tree out here, a bit a cell,
//! block by block, with how tall the tallest of a block's trees stands; once
//! the trees' prototypes grow, the survey bounds how high the crowns over
//! each block rise. A ray walks the cells it crosses while it
//! is low enough to reach a crown and meets each marked cell's tree, read
//! again from its place alone, once, as it comes within reach: no crown
//! reaches further than the walk looks about the cell it is in, so a hit
//! before the ray leaves that cell is the nearest.

use alloc::vec::Vec;
use core::ops::{ControlFlow, RangeInclusive};

use tairix_parallel::JobRunner;
use tairix_util::{fallible, mathf};

use crate::heightfield::Heightfield;
use crate::land::Grids;
use crate::prototype::Prototype;
use crate::shape::{meet_placed, occluded_by_placed, reciprocal, Aabb, Geometry, Hit};
use crate::vector::{above, below, share, wrapped, Pose, Ray, Vec3};
use crate::walk::{self, Stepped, Walk};
use crate::wood::{Habit, Offer, Reader, Sown, Tree};

/// The most cells a ray walks across one tile: past any a tile holds along a
/// ray, so only a walk gone wrong ever meets it.
const MOST_CELLS: u32 = 1 << 16;

/// How many tiles a wood's square is cut into a side, each traced as an
/// object of its own so a ray passing far from one never walks it.
pub(crate) const TILES: u32 = 24;

/// How many cells a side a block of a wood's marks and survey holds, and the
/// words of a bit a cell that mark which of them keep a tree.
const BLOCK: u32 = 16;
const WORDS: usize = (BLOCK * BLOCK / u64::BITS) as usize;
type Marks = [u64; WORDS];

/// How many blocks one step of a survey reads, and how many a core takes at
/// once.
const SURVEY_UNIT: usize = 1 << 8;
const SURVEY_BAND: usize = 1 << 3;

/// A wood far off.
#[derive(Clone, Debug)]
pub(crate) struct FarWood {
    reader: Reader,
    grids: Grids,
    /// The lattice its places are sown on, the land's cell its own first
    /// cell is, and where that cell's corner lies.
    sown: Sown,
    first: (i64, i64),
    origin: (f64, f64),
    /// Where it is seen from, the way the eye looks, how far either side of
    /// the view it stands, in radians, and how far off it ends.
    eye: (f64, f64),
    heading: f64,
    across: f64,
    to: f64,
    /// The square the land's grids trace, beyond which no ground lies.
    traced: ((f64, f64), f64),
    /// How many cells about its own a tree's crown may reach.
    spread: u32,
    /// How far its widest crown reaches, how tall its tallest tree stands
    /// over the ground, and the most any tree's top rises over the height it
    /// was asked to grow to.
    reach: f64,
    tallest: f64,
    rise: f64,
    /// The cells its wood's thinning keeps a tree in.
    marking: Marking,
    /// Its survey, once taken.
    survey: Option<Survey>,
}

/// The cells of a wood's lattice its thinning keeps a tree in, [`BLOCK`]
/// cells a side at a time over the ground its stretch across the view covers:
/// the first block's column and row, how many blocks a side the marking
/// spans, each block's marks, and how tall the tallest of its trees stands,
/// `-∞` where none does.
#[derive(Clone, Debug)]
struct Marking {
    first: (u32, u32),
    across: (u32, u32),
    marks: Vec<Marks>,
    tallest: Vec<f32>,
}

impl Marking {
    /// No cell marked over the blocks `across` a side from `first`; `None`
    /// when the heap will not hold them.
    fn new((first, across): ((u32, u32), (u32, u32))) -> Option<Self> {
        let blocks = usize::try_from(u64::from(across.0) * u64::from(across.1)).ok()?;
        Some(Self {
            first,
            across,
            marks: fallible::filled(blocks, [0; WORDS])?,
            tallest: fallible::filled(blocks, f32::NEG_INFINITY)?,
        })
    }

    /// Where block `block`'s records lie, if it lies within the marking.
    fn index(&self, block: (u32, u32)) -> Option<usize> {
        let (x, z) = (
            block.0.checked_sub(self.first.0)?,
            block.1.checked_sub(self.first.1)?,
        );
        if x >= self.across.0 || z >= self.across.1 {
            return None;
        }
        usize::try_from(u64::from(z) * u64::from(self.across.0) + u64::from(x)).ok()
    }

    /// Which word of its block's marks holds cell `cell`'s bit, and the bit.
    fn bit((column, row): (u32, u32)) -> (usize, u64) {
        let bit = (row % BLOCK) * BLOCK + column % BLOCK;
        (
            usize::try_from(bit / u64::BITS).unwrap_or(WORDS),
            1 << (bit % u64::BITS),
        )
    }

    /// Mark cell `cell` as keeping a tree `height` tall; `None` for a cell
    /// off the marking.
    fn mark(&mut self, cell: (u32, u32), height: f32) -> Option<()> {
        let index = self.index(block_of(cell))?;
        let (word, bit) = Self::bit(cell);
        *self.marks.get_mut(index)?.get_mut(word)? |= bit;
        let tallest = self.tallest.get_mut(index)?;
        *tallest = tallest.max(height);
        Some(())
    }

    /// Whether cell `cell` keeps a tree.
    fn marked(&self, cell: (u32, u32)) -> bool {
        let (word, bit) = Self::bit(cell);
        self.index(block_of(cell))
            .and_then(|index| self.marks.get(index))
            .and_then(|marks| marks.get(word))
            .is_some_and(|word| word & bit != 0)
    }

    /// How tall the tallest tree in block `block` stands; `None` where none
    /// does.
    fn tallest(&self, block: (u32, u32)) -> Option<f32> {
        let tallest = *self.tallest.get(self.index(block)?)?;
        tallest.is_finite().then_some(tallest)
    }

    /// The cells it marks, block by block.
    #[cfg(test)]
    fn cells(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        let width = self.across.0.max(1);
        self.marks
            .iter()
            .enumerate()
            .flat_map(move |(index, marks)| {
                let index = u32::try_from(index).unwrap_or(u32::MAX);
                let block = (self.first.0 + index % width, self.first.1 + index / width);
                (0..BLOCK * BLOCK).filter_map(move |bit| {
                    let word = marks.get(usize::try_from(bit / u64::BITS).ok()?)?;
                    (word & (1 << (bit % u64::BITS)) != 0)
                        .then_some((block.0 * BLOCK + bit % BLOCK, block.1 * BLOCK + bit / BLOCK))
                })
            })
    }
}

/// A wood's ground surveyed [`BLOCK`] cells a side at a time over its
/// stretch across the view and as far again as a crown reaches: the first
/// block's column and row, how many blocks a side the survey spans, and each
/// block's record.
#[derive(Clone, Debug)]
struct Survey {
    first: (u32, u32),
    across: (u32, u32),
    blocks: Vec<Block>,
}

/// A block of a wood's survey: the lowest its ground lies and the highest any
/// crown reaching over it stands, `-∞` where none does, each rounded outward.
#[derive(Copy, Clone, Debug)]
struct Block {
    low: f32,
    high: f32,
}

/// A block not yet read.
const UNREAD: Block = Block {
    low: f32::NEG_INFINITY,
    high: f32::NEG_INFINITY,
};

/// The block of a wood's lattice cell `cell` lies in.
const fn block_of((column, row): (u32, u32)) -> (u32, u32) {
    (column / BLOCK, row / BLOCK)
}

impl Survey {
    /// The survey of the blocks `across` a side from `first`, each as `own`
    /// reads its own ground and trees, its crowns reaching over the blocks
    /// `reach` about it, found across `runner`; `None` when the heap will not
    /// hold it.
    fn reaching(
        (first, across): ((u32, u32), (u32, u32)),
        own: &[Block],
        (reach, runner): (u32, &dyn JobRunner),
    ) -> Option<Self> {
        let width = usize::try_from(across.0).ok()?;
        let height = usize::try_from(across.1).ok()?;
        let reach = usize::try_from(reach).ok()?;
        let mut blocks = fallible::filled(own.len(), UNREAD)?;
        tairix_parallel::for_each_band(runner, &mut blocks, (0, width.max(1)), &|z, row| {
            let rows = z.saturating_sub(reach)..=(z + reach).min(height.saturating_sub(1));
            for (x, slot) in row.iter_mut().enumerate() {
                let columns = x.saturating_sub(reach)..=(x + reach).min(width.saturating_sub(1));
                slot.high = rows
                    .clone()
                    .flat_map(|z| columns.clone().map(move |x| z * width + x))
                    .filter_map(|at| own.get(at))
                    .map(|about| about.high)
                    .fold(f32::NEG_INFINITY, f32::max);
                slot.low = own
                    .get(z * width + x)
                    .map_or(f32::NEG_INFINITY, |block| block.low);
            }
        });
        Some(Self {
            first,
            across,
            blocks,
        })
    }

    /// The block cell `cell` lies in, and its record: `None` off the survey,
    /// where no tree stands.
    fn of(&self, cell: (u32, u32)) -> ((u32, u32), Option<Block>) {
        let block = block_of(cell);
        let (x, z) = (
            block.0.wrapping_sub(self.first.0),
            block.1.wrapping_sub(self.first.1),
        );
        if x >= self.across.0 || z >= self.across.1 {
            return (block, None);
        }
        let record = usize::try_from(u64::from(z) * u64::from(self.across.0) + u64::from(x))
            .ok()
            .and_then(|index| self.blocks.get(index).copied());
        (block, record)
    }

    /// The highest any crown reaching over the blocks from `low` to `high`,
    /// both taken, stands; `None` where none does.
    fn highest_over(&self, (low, high): ((u32, u32), (u32, u32))) -> Option<f64> {
        let within = |(from, to): (u32, u32),
                      (first, across): (u32, u32)|
         -> Option<RangeInclusive<usize>> {
            let last = first.checked_add(across.checked_sub(1)?)?;
            if to < first || from > last {
                return None;
            }
            Some(
                usize::try_from(from.max(first) - first).ok()?
                    ..=usize::try_from(to.min(last) - first).ok()?,
            )
        };
        let columns = within((low.0, high.0), (self.first.0, self.across.0))?;
        let rows = within((low.1, high.1), (self.first.1, self.across.1))?;
        let width = usize::try_from(self.across.0).ok()?;
        let highest = rows
            .flat_map(|z| columns.clone().map(move |x| z * width + x))
            .filter_map(|index| self.blocks.get(index))
            .map(|block| f64::from(block.high))
            .fold(f64::NEG_INFINITY, f64::max);
        highest.is_finite().then_some(highest)
    }
}

/// A wood far off being surveyed a run of its blocks at a time: the wood,
/// the first block's column and row, how many blocks a side the survey spans
/// and in all, and each block read so far, row by row, room made for the
/// rest.
#[derive(Debug)]
pub(crate) struct Surveying {
    wood: FarWood,
    first: (u32, u32),
    across: (u32, u32),
    width: usize,
    blocks: usize,
    own: Vec<Block>,
}

impl Surveying {
    /// How far the survey has come, `0.0..=1.0`: its blocks read, and then
    /// what they read gathered into the survey.
    pub(crate) fn done(&self) -> f64 {
        share(self.own.len(), self.blocks + 1)
    }

    /// Read the next run of its blocks across `runner`, on the land in
    /// `fields`, or once every block is read gather them into the survey:
    /// the survey to carry on with, or the wood surveyed; `None` when the
    /// heap will not hold its survey.
    pub(crate) fn step(
        mut self,
        fields: &[Heightfield],
        runner: &dyn JobRunner,
    ) -> Option<ControlFlow<FarWood, Self>> {
        if self.own.len() < self.blocks {
            let start = self.own.len();
            let end = (start + SURVEY_UNIT).min(self.blocks);
            // Within the room made at the start, so it allocates nothing.
            self.own.resize(end, UNREAD);
            let (first, width, wood) = (self.first, self.width.max(1), &self.wood);
            let blocks = self.own.get_mut(start..end)?;
            tairix_parallel::for_each_band(runner, blocks, (0, SURVEY_BAND), &|number, band| {
                for (offset, slot) in band.iter_mut().enumerate() {
                    let index = start + number * SURVEY_BAND + offset;
                    let at = |at: usize, first: u32| {
                        first.saturating_add(u32::try_from(at).unwrap_or(u32::MAX))
                    };
                    let block = (at(index % width, first.0), at(index / width, first.1));
                    *slot = wood.own(block, fields);
                }
            });
            return Some(ControlFlow::Continue(self));
        }
        let Self {
            mut wood,
            first,
            across,
            own,
            ..
        } = self;
        wood.survey = Some(Survey::reaching(
            (first, across),
            &own,
            (wood.spread.div_ceil(BLOCK), runner),
        )?);
        Some(ControlFlow::Break(wood))
    }
}

/// How a ray crosses a block of a wood's survey.
enum Crossing {
    /// Over every crown reaching it, or where none does: on to where it
    /// leaves the block, from block `block`.
    Clear((u32, u32), f64),
    /// Under the ground all across it: nothing beyond is seen.
    Under,
    /// Low enough a crown may stand in its way.
    Within,
}

/// How many cells about its own a crown reaching `reach` may reach, on a
/// lattice of `cell`.
fn spread(reach: f64, cell: f64) -> Option<u32> {
    u32::try_from(mathf::round_i32(mathf::ceil(reach / cell)).max(1)).ok()
}

/// A square of a wood's ground traced as an object of its own: its corners,
/// the lowest and highest the ground stands there, and the highest any crown
/// over it stands.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Tile {
    pub(crate) from: (f64, f64),
    pub(crate) to: (f64, f64),
    pub(crate) ground: (f64, f64),
    pub(crate) crowns: f64,
}

impl Tile {
    /// The box its ground and the crowns over it lie within.
    pub(crate) fn bounds(&self) -> Aabb {
        Aabb {
            min: Vec3::new(self.from.0, self.ground.0 - 1.0, self.from.1),
            max: Vec3::new(self.to.0, self.crowns + 1.0, self.to.1),
        }
    }
}

/// Where a wood far off stands.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Stretch {
    /// Where it is seen from and the way the eye looks.
    pub(crate) eye: (f64, f64),
    pub(crate) heading: f64,
    /// How far either side of the view it stands, in radians.
    pub(crate) across: f64,
    /// How far off it ends.
    pub(crate) to: f64,
}

impl FarWood {
    /// The trees `reader` grows on the lattice `sown` over `stretch` of the
    /// land `grids` trace in `fields`, none marked yet; `None` if it holds no
    /// kind or no ground, or the heap will not hold its marks.
    pub(crate) fn new(
        (reader, sown): (Reader, Sown),
        (grids, fields): (Grids, &[Heightfield]),
        stretch: Stretch,
    ) -> Option<Self> {
        let tallest = reader.kinds().map(Habit::tallest).fold(0.0, mathf::fmax);
        let reach = reader
            .kinds()
            .map(|habit| habit.crown * habit.tallest())
            .fold(0.0, mathf::fmax);
        let usable = |length: f64| length.is_finite() && length > 0.0;
        if tallest <= 0.0 || !usable(sown.side) || !usable(stretch.to) {
            return None;
        }
        let traced = grids.traced(fields);
        let ((cx, cz), half) = traced;
        let first = sown.cell((cx - half, cz - half))?;
        let corner = |cell: i64| Some(f64::from(i32::try_from(cell).ok()?) * sown.side);
        let mut wood = Self {
            reader,
            grids,
            sown,
            first,
            origin: (corner(first.0)?, corner(first.1)?),
            eye: stretch.eye,
            heading: stretch.heading,
            across: stretch.across,
            to: stretch.to,
            traced,
            spread: spread(reach, sown.side)?,
            reach,
            tallest,
            rise: 1.0,
            marking: Marking::new(((0, 0), (0, 0)))?,
            survey: None,
        };
        // A cell over, so no place jittered to the edge of its cell is lost.
        wood.marking = Marking::new(wood.blocks(wood.extent(sown.side)))?;
        Some(wood)
    }

    /// Mark the place `offer` at the land's cell `cell` as keeping its tree
    /// out here; `None` for a cell off its marking, which no place across its
    /// stretch lies in.
    pub(crate) fn mark(&mut self, cell: (i64, i64), offer: &Offer) -> Option<()> {
        let local = |along: i64, first: i64| u32::try_from(along.checked_sub(first)?).ok();
        let cell = (local(cell.0, self.first.0)?, local(cell.1, self.first.1)?);
        self.marking.mark(cell, offer.height)
    }

    /// The bark of its first kind: what its tiles are made in, its trees'
    /// parts made in their own.
    pub(crate) fn bark(&self) -> Option<usize> {
        self.reader.kinds().next().map(|habit| habit.bark)
    }

    /// The tiles it is traced as: those of its square's that it reaches and,
    /// once it is surveyed, that a crown stands over, each bounded by the
    /// ground and the crowns over it.
    pub(crate) fn tiles<'a>(
        &'a self,
        fields: &'a [Heightfield],
    ) -> impl Iterator<Item = Tile> + 'a {
        let ((cx, cz), half) = self.traced;
        let side = 2.0 * half / f64::from(TILES);
        let span = f64::from(BLOCK) * self.sown.side;
        let block =
            move |at: f64, origin: f64| tairix_noise::cell(((at - origin) / span).max(0.0)).0;
        (0..TILES * TILES).filter_map(move |index| {
            let (column, row) = (index % TILES, index / TILES);
            let from = (
                cx - half + f64::from(column) * side,
                cz - half + f64::from(row) * side,
            );
            let to = (from.0 + side, from.1 + side);
            // A crown standing at its edge may reach past it.
            if !self.reaches((from, to), self.reach) {
                return None;
            }
            let ground = self.grids.extremes(fields, (from, to));
            let crowns = match &self.survey {
                Some(survey) => survey.highest_over((
                    (block(from.0, self.origin.0), block(from.1, self.origin.1)),
                    (block(to.0, self.origin.0), block(to.1, self.origin.1)),
                ))?,
                None => ground.1 + self.tallest,
            };
            Some(Tile {
                from,
                to,
                ground,
                crowns,
            })
        })
    }

    /// Take the measure of its trees as their prototypes grew, from
    /// `prototypes`: how high each stands and how far its limbs spread from
    /// its trunk for the height it was asked to grow to, which bound what a
    /// walk looks for.
    fn measure(&mut self, prototypes: &[Prototype]) -> Option<()> {
        for habit in self.reader.kinds() {
            for (&prototype, &natural) in habit.prototypes.iter().zip(&habit.heights) {
                let bounds = prototypes.get(prototype as usize)?.bounds();
                let across = [bounds.min.x, bounds.max.x, bounds.min.z, bounds.max.z]
                    .into_iter()
                    .map(f64::abs)
                    .fold(0.0, f64::max);
                // Turned any way about its trunk, a corner of its box swings
                // out to the box's diagonal.
                let reach = core::f64::consts::SQRT_2 * across * habit.scaled.1;
                self.rise = self.rise.max(bounds.max.y / natural.max(1e-3));
                self.reach = self.reach.max(reach);
                self.tallest = self.tallest.max(bounds.max.y * habit.scaled.1);
            }
        }
        self.spread = spread(self.reach, self.sown.side)?;
        Some(())
    }

    /// Its survey begun once its trees are measured as their prototypes in
    /// `prototypes` grew: its ground to be read a block at a time for the
    /// lowest it lies under each and the highest any crown reaching over it
    /// stands. `None` when the heap will not hold it.
    pub(crate) fn surveying(mut self, prototypes: &[Prototype]) -> Option<Surveying> {
        self.measure(prototypes)?;
        // Over every cell marked, however little its crowns reach.
        let (first, across) = self.blocks(self.extent(self.reach + self.sown.side));
        let width = usize::try_from(across.0).ok()?;
        let blocks = width.checked_mul(usize::try_from(across.1).ok()?)?;
        let mut own = Vec::new();
        own.try_reserve_exact(blocks).ok()?;
        Some(Surveying {
            wood: self,
            first,
            across,
            width,
            blocks,
            own,
        })
    }

    /// The first block of its lattice the ground from `low` to `high` lies in,
    /// and how many blocks a side it spans: none where it is empty.
    fn blocks(&self, (low, high): ((f64, f64), (f64, f64))) -> ((u32, u32), (u32, u32)) {
        let span = f64::from(BLOCK) * self.sown.side;
        let index = |at: f64, origin: f64| tairix_noise::cell(((at - origin) / span).max(0.0)).0;
        let first = (index(low.0, self.origin.0), index(low.1, self.origin.1));
        if low.0 > high.0 || low.1 > high.1 {
            return (first, (0, 0));
        }
        let last = (index(high.0, self.origin.0), index(high.1, self.origin.1));
        (
            first,
            (
                last.0.saturating_sub(first.0) + 1,
                last.1.saturating_sub(first.1) + 1,
            ),
        )
    }

    /// The corners of the ground its stretch across the view covers, grown
    /// `pad` every way, within the traced square: empty where the two do not
    /// meet.
    fn extent(&self, pad: f64) -> ((f64, f64), (f64, f64)) {
        let (ex, ez) = self.eye;
        let (mut low, mut high) = (self.eye, self.eye);
        let mut take = |angle: f64| {
            let (x, z) = (
                ex + self.to * mathf::sin(angle),
                ez + self.to * mathf::cos(angle),
            );
            low = (mathf::fmin(low.0, x), mathf::fmin(low.1, z));
            high = (mathf::fmax(high.0, x), mathf::fmax(high.1, z));
        };
        // The eye, its far edge's ends, and that edge's furthest along either
        // axis wherever that lies across the view.
        for side in [-1.0, 1.0] {
            take(self.heading + side * self.across);
        }
        for quarter in 0..4u8 {
            let angle = f64::from(quarter) * core::f64::consts::FRAC_PI_2;
            if wrapped(angle - self.heading).abs() <= self.across {
                take(angle);
            }
        }
        let ((cx, cz), half) = self.traced;
        (
            (
                mathf::fmax(low.0 - pad, cx - half),
                mathf::fmax(low.1 - pad, cz - half),
            ),
            (
                mathf::fmin(high.0 + pad, cx + half),
                mathf::fmin(high.1 + pad, cz + half),
            ),
        )
    }

    /// Block `(x, z)` as its own ground and trees read on the land in
    /// `fields`: the lowest its ground lies, and the highest any tree marked
    /// in it could rise — its tallest, rooted no higher than the block's
    /// highest ground, by the most a top rises over the height it was asked
    /// to grow to.
    fn own(&self, (x, z): (u32, u32), fields: &[Heightfield]) -> Block {
        let span = f64::from(BLOCK) * self.sown.side;
        let from = (
            self.origin.0 + f64::from(x) * span,
            self.origin.1 + f64::from(z) * span,
        );
        let (low, ground) = self
            .grids
            .extremes(fields, (from, (from.0 + span, from.1 + span)));
        let high = self
            .marking
            .tallest((x, z))
            .filter(|_| ground.is_finite())
            .map_or(f32::NEG_INFINITY, |tallest| {
                above(ground + self.rise * f64::from(tallest))
            });
        Block {
            low: if low.is_finite() {
                below(low)
            } else {
                f32::NEG_INFINITY
            },
            high,
        }
    }

    /// How `ray`, come `t` along it into cell `cell`, crosses the cell's
    /// block, by its survey; low enough to meet anything there until the
    /// wood is surveyed.
    fn crossing(&self, ray: &Ray, (cell, t): ((u32, u32), f64)) -> Crossing {
        let Some(survey) = self.survey.as_ref() else {
            return Crossing::Within;
        };
        let (block, record) = survey.of(cell);
        let (low, high) = record.map_or((f64::NEG_INFINITY, f64::NEG_INFINITY), |block| {
            (f64::from(block.low), f64::from(block.high))
        });
        let span = f64::from(BLOCK) * self.sown.side;
        let leaves = |at: u32, origin: f64, (start, dir): (f64, f64)| {
            let from = origin + f64::from(at) * span;
            if dir > 1e-12 {
                (from + span - start) / dir
            } else if dir < -1e-12 {
                (from - start) / dir
            } else {
                f64::INFINITY
            }
        };
        let out = leaves(block.0, self.origin.0, (ray.origin.x, ray.dir.x))
            .min(leaves(block.1, self.origin.1, (ray.origin.z, ray.dir.z)))
            .max(t);
        let (entering, leaving) = (
            ray.origin.y + ray.dir.y * t,
            ray.origin.y + ray.dir.y * out.min(1e30),
        );
        if entering.max(leaving) < low {
            Crossing::Under
        } else if entering.min(leaving) > high {
            Crossing::Clear(block, out)
        } else {
            Crossing::Within
        }
    }

    /// Whether any of the square `from`–`to`, grown `pad` every way, might
    /// hold its trees: whether it overlaps its stretch across the view.
    fn reaches(&self, (from, to): ((f64, f64), (f64, f64)), pad: f64) -> bool {
        let (from, to) = ((from.0 - pad, from.1 - pad), (to.0 + pad, to.1 + pad));
        let (ex, ez) = self.eye;
        let nearest = mathf::hypot(
            (from.0 - ex).max(ex - to.0).max(0.0),
            (from.1 - ez).max(ez - to.1).max(0.0),
        );
        let corners = [
            (from.0, from.1),
            (to.0, from.1),
            (from.0, to.1),
            (to.0, to.1),
        ];
        if nearest > self.to {
            return false;
        }
        // Across the view: some corner, or the middle, within it, or the
        // square holding the eye.
        let middle = (f64::midpoint(from.0, to.0), f64::midpoint(from.1, to.1));
        let across = |(x, z): (f64, f64)| {
            wrapped(mathf::atan2(x - ex, z - ez) - self.heading).abs() <= self.across
        };
        nearest <= 0.0 || across(middle) || corners.iter().any(|&corner| across(corner)) || {
            // A square wide across the view's edge, its corners either side.
            let half = 0.5 * mathf::hypot(to.0 - from.0, to.1 - from.1);
            let distance = mathf::hypot(middle.0 - ex, middle.1 - ez);
            let off = wrapped(mathf::atan2(middle.0 - ex, middle.1 - ez) - self.heading).abs();
            distance * mathf::sin((off - self.across).max(0.0)) <= half
        }
    }

    /// Whether a tree stands in cell `cell`, and if one does, the highest any
    /// crown about it stands: as its survey has it once taken, and possibly
    /// anywhere, however high, until then.
    fn standing(&self, cell: (u32, u32)) -> Option<f64> {
        self.marking.marked(cell).then(|| {
            self.survey
                .as_ref()
                .and_then(|survey| survey.of(cell).1)
                .map_or(f64::INFINITY, |block| f64::from(block.high))
        })
    }

    /// The place cell `cell` of its lattice holds, and its draws: the land's
    /// own, as its wood's thinning read it.
    fn place(&self, (column, row): (u32, u32)) -> Option<((f64, f64), u32)> {
        self.sown.place((
            self.first.0 + i64::from(column),
            self.first.1 + i64::from(row),
        ))
    }

    /// The prototype `tree`, grown at the place `(at, draw)`, is placed from,
    /// at its pose, scale and key, and the bark its limbs are made of.
    fn placing_of(
        &self,
        tree: &Tree,
        (at, draw): ((f64, f64), u32),
    ) -> Option<((u32, Pose, f64, u32), usize)> {
        let (prototype, bark) = self.grown_from(tree)?;
        let (pose, key) = tree.placing(at, draw);
        Some(((prototype, pose, tree.scale, key), bark))
    }

    /// The prototype `tree` is grown from, and the bark its limbs are made of.
    fn grown_from(&self, tree: &Tree) -> Option<(u32, usize)> {
        let habit = self.reader.kinds().nth(usize::from(tree.kind))?;
        Some((
            *habit.prototypes.get(usize::from(tree.variant))?,
            habit.bark,
        ))
    }

    /// The prototype the tree cell `cell` keeps is placed from, as
    /// [`Self::placing_of`] places it, its ground read in `fields`.
    fn placed(
        &self,
        cell: (u32, u32),
        fields: &[Heightfield],
    ) -> Option<((u32, Pose, f64, u32), usize)> {
        let place = self.place(cell)?;
        let tree = self.reader.tree((&self.grids, fields), place, None)?;
        self.placing_of(&tree, place)
    }

    /// Where the tree a hit on cell `cell` met stands, and its key: what its
    /// pattern is fixed in and set apart by.
    pub(crate) fn placing(&self, cell: (u32, u32), fields: &[Heightfield]) -> Option<(Pose, u32)> {
        self.placed(cell, fields)
            .map(|((_, pose, _, key), _)| (pose, key))
    }

    /// The nearest place in `(near, far)` where `ray` meets one of the trees
    /// of `tile`.
    pub(crate) fn intersect(
        &self,
        tile: &Tile,
        ray: &Ray,
        span: (f64, f64),
        geometry: Geometry<'_>,
    ) -> Option<Hit> {
        self.walk(tile, ray, span, geometry, false)
    }

    /// Whether `ray` meets any of the trees of `tile` in `(near, far)`.
    pub(crate) fn occludes(
        &self,
        tile: &Tile,
        ray: &Ray,
        span: (f64, f64),
        geometry: Geometry<'_>,
    ) -> bool {
        self.walk(tile, ray, span, geometry, true).is_some()
    }

    /// Walk `ray` across `tile`'s cells within `(near, far)` for the nearest
    /// tree it meets, or for any where `any`: past every block of the survey
    /// it crosses over every crown, and no further than the ground.
    fn walk(
        &self,
        tile: &Tile,
        ray: &Ray,
        (near, far): (f64, f64),
        geometry: Geometry<'_>,
        any: bool,
    ) -> Option<Hit> {
        let (enter, leave) = tile.bounds().span(ray, reciprocal(ray.dir), far)?;
        let mut t = self.comes_down(ray, (enter.max(near), leave), geometry.fields)?;
        let mut walk = Walk::from((self.origin, self.sown.side), ray, t);
        let mut meeting = Meeting {
            wood: self,
            ray,
            geometry,
            near,
            reach: far,
            best: None,
            any,
        };
        // Whether the trees about the walk's cell are still to be met, as
        // they are once it comes into a block a crown may reach.
        let mut arrived = true;
        for _ in 0..MOST_CELLS {
            match self.crossing(ray, (walk.cell, t)) {
                Crossing::Under => return meeting.best,
                Crossing::Clear(block, out) => {
                    if out >= leave.min(meeting.reach) {
                        return meeting.best;
                    }
                    t = out;
                    walk = Walk::from((self.origin, self.sown.side), ray, t);
                    // Set down on the block's own wall: on across it.
                    while block_of(walk.cell) == block {
                        let next = walk.exit();
                        if next >= leave {
                            return meeting.best;
                        }
                        walk.step();
                        t = next;
                    }
                    arrived = true;
                    continue;
                }
                Crossing::Within => {}
            }
            if arrived {
                meeting.around(walk.cell);
                arrived = false;
            }
            let exit = walk.exit().min(leave);
            if meeting.best.is_some_and(|hit| any || hit.t <= exit) {
                return meeting.best;
            }
            if exit >= leave.min(meeting.reach) {
                return meeting.best;
            }
            let stepped = walk.step();
            t = exit;
            meeting.entering(walk.cell, stepped);
        }
        meeting.best
    }

    /// Where `ray` first comes within reach of a crown in `(from, to)`: near
    /// enough the ground that a tree's crown, standing where the ground rises
    /// up to its reach away, could stand over it.
    fn comes_down(&self, ray: &Ray, span: (f64, f64), fields: &[Heightfield]) -> Option<f64> {
        self.grids
            .approach(fields, ray, self.tallest + self.reach, span)
    }
}

/// One ray's meetings with a wood's trees, nearest kept.
struct Meeting<'a> {
    wood: &'a FarWood,
    ray: &'a Ray,
    geometry: Geometry<'a>,
    near: f64,
    reach: f64,
    best: Option<Hit>,
    any: bool,
}

impl Meeting<'_> {
    /// Meet every tree whose crown could reach `cell`.
    fn around(&mut self, cell: (u32, u32)) {
        let spread = self.wood.spread;
        for about in walk::about(cell, (spread, spread)) {
            self.meet(about);
        }
    }

    /// Meet the trees that come within reach as the walk steps into `cell`
    /// as `stepped` has it: a column of cells across x, or a row across z.
    fn entering(&mut self, cell: (u32, u32), stepped: Stepped) {
        let spread = self.wood.spread;
        for ahead in walk::ahead(cell, stepped, (spread, spread)) {
            self.meet(ahead);
        }
    }

    /// Meet the tree `cell` holds, if one stands there and the ray passes
    /// within its reach: the ground there read only where the ray passes
    /// beneath the crowns about it, and the tree grown only where it passes
    /// low enough to meet the tallest the place alone could grow.
    fn meet(&mut self, cell: (u32, u32)) {
        if self.any && self.best.is_some() {
            return;
        }
        let wood = self.wood;
        let Some(crowns) = wood.standing(cell) else {
            return;
        };
        let fields = self.geometry.fields;
        let Some(place @ (at, _)) = wood.place(cell) else {
            return;
        };
        let Some((first, last)) = walk::passing(self.ray, at, wood.reach, (self.near, self.reach))
        else {
            return;
        };
        let ray = self.ray;
        let lowest = (ray.origin.y + ray.dir.y * first).min(ray.origin.y + ray.dir.y * last);
        if lowest > crowns {
            return;
        }
        let ground = wood.grids.height(fields, at.0, at.1);
        // Over the tallest tree that grows anywhere, then over the tallest
        // this place could grow.
        if lowest > ground + wood.tallest {
            return;
        }
        let Some(sprout) = wood.reader.sprout(place, None) else {
            return;
        };
        if lowest > ground + wood.rise * wood.reader.highest(&sprout) {
            return;
        }
        let Some(tree) = wood.reader.grown(&sprout, (&wood.grids, fields)) else {
            return;
        };
        let Some((placing, bark)) = wood.placing_of(&tree, place) else {
            return;
        };
        let (prototype, pose, scale, key) = placing;
        let span = (self.near, self.reach);
        if self.any {
            if occluded_by_placed(
                (prototype, &pose, scale, key),
                self.ray,
                span,
                self.geometry,
            ) {
                self.best = Some(Hit::plain(self.near, Vec3::UP));
            }
            return;
        }
        let Some(mut hit) = meet_placed(
            (prototype, &pose, scale, key),
            self.ray,
            span,
            self.geometry,
        ) else {
            return;
        };
        if hit.material.is_none() {
            hit.material = u32::try_from(bark).ok();
        }
        hit.member = Some(cell);
        self.reach = hit.t;
        self.best = Some(hit);
    }
}

#[cfg(test)]
impl FarWood {
    /// Whether `at` lies across its view, however far off.
    pub(crate) fn across(&self, at: (f64, f64)) -> bool {
        let (dx, dz) = (at.0 - self.eye.0, at.1 - self.eye.1);
        wrapped(mathf::atan2(dx, dz) - self.heading).abs() <= self.across
    }

    /// Every tree it keeps, read again from its place on the land in
    /// `fields`.
    pub(crate) fn kept<'a>(
        &'a self,
        fields: &'a [Heightfield],
    ) -> impl Iterator<Item = Offer> + 'a {
        self.marking
            .cells()
            .filter_map(move |cell| self.offered(cell, fields))
    }

    /// What the cell of its lattice holding `at` offers on the land in
    /// `fields`, as its wood's thinning read it.
    #[cfg(feature = "scene-tests")]
    pub(crate) fn offered_at(&self, at: (f64, f64), fields: &[Heightfield]) -> Option<Offer> {
        let (column, row) = self.sown.cell(at)?;
        let local = |along: i64, first: i64| u32::try_from(along - first).ok();
        self.offered(
            (local(column, self.first.0)?, local(row, self.first.1)?),
            fields,
        )
    }

    /// What cell `cell` of its lattice offers on the land in `fields`.
    fn offered(&self, cell: (u32, u32), fields: &[Heightfield]) -> Option<Offer> {
        let place = self.place(cell)?;
        let tree = self.reader.tree((&self.grids, fields), place, None)?;
        Some(Offer::of(&tree, place))
    }
}

#[cfg(test)]
#[path = "far_wood_tests.rs"]
mod tests;

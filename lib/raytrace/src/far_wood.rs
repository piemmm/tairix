//! A wood carried on far off: its trees hashed from the cells of a lattice
//! over the land rather than stood one by one, so a wood reaches as far as
//! its trees still span a pixel for no more than the walk a ray takes across
//! it.
//!
//! Each cell holds one place a tree might stand, jittered within it; the
//! wood's reading decides whether a tree grows there and what it is, as it
//! decides for the trees stood about the eye, and a place keeps its tree with
//! the odds the room its crown asks for leaves it, so the wood stands as
//! thickly far off as thinning leaves it near. Its survey reads each place's
//! tree once, keeping a bit a cell for those that stand one and how high the
//! crowns over each block of cells rise. A ray walks the cells it crosses
//! while it is low enough to reach a crown and meets each tree whose crown
//! could reach them once, as it comes within reach: no crown reaches further
//! than the walk looks about the cell it is in, so a hit before the ray
//! leaves that cell is the nearest.

use alloc::vec::Vec;
use core::ops::{ControlFlow, RangeInclusive};

use tairix_parallel::JobRunner;
use tairix_util::{fallible, mathf};

use crate::band;
use crate::heightfield::Heightfield;
use crate::land::Grids;
use crate::noise::hash3;
use crate::prototype::Prototype;
use crate::sample::{mix32, unit};
use crate::shape::{meet_placed, occluded_by_placed, reciprocal, Aabb, Geometry, Hit};
use crate::vector::{above, below, real, share, wrapped, Pose, Ray, Vec3};
use crate::walk::{self, Stepped, Walk};
use crate::wood::{Habit, Reader, Sprout, Tree, PACKED};

/// How far short of where it begins a wood far off is matched to the trees
/// stood one by one before it, as a share of that distance, and how many of
/// the cells there it reads, one in so many each way.
const MATCHED: f64 = 0.6;
const SAMPLED: u32 = 2;

/// How many of those places one step of matching reads, and how many a core
/// takes at once.
const MATCH_UNIT: usize = 1 << 14;
const MATCH_BAND: usize = 1 << 8;

/// How many times the share of the ground a wood's crowns fill is halved
/// toward the one that matches: far finer than the ring's sampling tells.
const BISECTED: u32 = 16;

/// The most cells a ray walks across one tile: past any a tile holds along a
/// ray, so only a walk gone wrong ever meets it.
const MOST_CELLS: u32 = 1 << 16;

/// How many tiles a wood's square is cut into a side, each traced as an
/// object of its own so a ray passing far from one never walks it.
pub(crate) const TILES: u32 = 24;

/// How many cells a side a block of a wood's survey holds, and the words of
/// a bit a cell that mark which of them keep a tree.
const BLOCK: u32 = 16;
const WORDS: usize = (BLOCK * BLOCK / u64::BITS) as usize;
type Marks = [u64; WORDS];

/// Where a block's marks lie among its survey's when no tree stands in it.
const UNMARKED: u32 = u32::MAX;

/// How many blocks one step of a survey reads, and how many a core takes at
/// once.
const SURVEY_UNIT: usize = 1 << 8;
const SURVEY_BAND: usize = 1 << 3;

/// A wood far off.
#[derive(Clone, Debug)]
pub(crate) struct FarWood {
    reader: Reader,
    grids: Grids,
    /// Where it is seen from, the way the eye looks, and how far either side
    /// of the view it stands, in radians; that way as a unit step across the
    /// ground, and the cosine of how far either side.
    eye: (f64, f64),
    heading: f64,
    across: f64,
    facing: (f64, f64),
    widest: f64,
    /// How far off it begins, where the trees stood one by one end, and how
    /// far off it ends.
    from: f64,
    to: f64,
    /// The square the land's grids trace, beyond which no ground lies.
    traced: ((f64, f64), f64),
    /// The lattice's first corner and its cell, how many cells about its
    /// own a tree's crown may reach, and the seed its places are drawn under.
    origin: (f64, f64),
    cell: f64,
    spread: u32,
    seed: u32,
    /// How closely the trees stood one by one before it were sown, and how
    /// much of the ground its crowns fill where they crowd, as thickly as
    /// theirs.
    sown: f64,
    filled: f64,
    /// How far its widest crown reaches, how tall its tallest tree stands
    /// over the ground, and the most any tree's top rises over the height it
    /// was asked to grow to.
    reach: f64,
    tallest: f64,
    rise: f64,
    /// Its survey, once taken.
    survey: Option<Survey>,
}

/// A wood's lattice surveyed [`BLOCK`] cells a side at a time over the
/// ground its ring across the view covers: the first block's column and row,
/// how many blocks a side the survey spans, each block's record, and the
/// marks of those a tree stands in.
#[derive(Clone, Debug)]
struct Survey {
    first: (u32, u32),
    across: (u32, u32),
    blocks: Vec<Block>,
    marks: Vec<Marks>,
}

/// A block of a wood's survey: the lowest its ground lies and the highest any
/// crown reaching over it stands, `-∞` where none does, each rounded outward;
/// and where its marks lie among the survey's, [`UNMARKED`] where no tree
/// stands in it.
#[derive(Copy, Clone, Debug)]
struct Block {
    low: f32,
    high: f32,
    marks: u32,
}

/// A block as its own cells alone read: the lowest its ground lies, the
/// highest a tree standing in it rises, and a bit a cell, row by row, set
/// where one stands.
#[derive(Copy, Clone, Debug)]
struct Own {
    low: f32,
    high: f32,
    marks: Marks,
}

/// A block not yet read.
const UNREAD: Own = Own {
    low: f32::NEG_INFINITY,
    high: f32::NEG_INFINITY,
    marks: [0; WORDS],
};

/// The block of a wood's survey cell `cell` lies in.
const fn block_of((column, row): (u32, u32)) -> (u32, u32) {
    (column / BLOCK, row / BLOCK)
}

impl Survey {
    /// The survey of the blocks `across` a side from `first`, each as `own`
    /// reads its own cells, its crowns reaching over the blocks `reach`
    /// about it, found across `runner`; `None` when the heap will not hold
    /// it.
    fn reaching(
        (first, across): ((u32, u32), (u32, u32)),
        own: &[Own],
        (reach, runner): (u32, &dyn JobRunner),
    ) -> Option<Self> {
        let width = usize::try_from(across.0).ok()?;
        let height = usize::try_from(across.1).ok()?;
        let reach = usize::try_from(reach).ok()?;
        let unmarked: Marks = [0; WORDS];
        let mut marks = Vec::new();
        marks
            .try_reserve_exact(own.iter().filter(|block| block.marks != unmarked).count())
            .ok()?;
        let empty = Block {
            low: f32::NEG_INFINITY,
            high: f32::NEG_INFINITY,
            marks: UNMARKED,
        };
        let mut blocks = fallible::filled(own.len(), empty)?;
        band::for_each(runner, &mut blocks, (0, width.max(1)), &|z, row| {
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
        for (block, read) in blocks.iter_mut().zip(own) {
            if read.marks != unmarked {
                block.marks = u32::try_from(marks.len())
                    .ok()
                    .filter(|&at| at != UNMARKED)?;
                marks.push(read.marks);
            }
        }
        Some(Self {
            first,
            across,
            blocks,
            marks,
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

    /// Whether a tree stands in cell `cell`, and if one does, the highest any
    /// crown about it stands.
    fn standing(&self, cell: (u32, u32)) -> Option<f64> {
        let block = self.of(cell).1?;
        let marks = self.marks.get(usize::try_from(block.marks).ok()?)?;
        let bit = (cell.1 % BLOCK) * BLOCK + cell.0 % BLOCK;
        let word = marks.get(usize::try_from(bit / u64::BITS).ok()?)?;
        ((word >> (bit % u64::BITS)) & 1 == 1).then_some(f64::from(block.high))
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
    own: Vec<Own>,
}

impl Surveying {
    /// How far the survey has come, `0.0..=1.0`: its blocks read, and then
    /// what they read gathered into the survey.
    pub(crate) fn done(&self) -> f64 {
        share(self.own.len(), self.blocks + 1)
    }

    /// Read the next run of its blocks across `runner`, on the land in
    /// `fields` with the wood's trees grown from `prototypes`, or once every
    /// block is read gather them into the survey: the survey to carry on
    /// with, or the wood surveyed; `None` when the heap will not hold its
    /// survey.
    pub(crate) fn step(
        mut self,
        (fields, prototypes): (&[Heightfield], &[Prototype]),
        runner: &dyn JobRunner,
    ) -> Option<ControlFlow<FarWood, Self>> {
        if self.own.len() < self.blocks {
            let start = self.own.len();
            let end = (start + SURVEY_UNIT).min(self.blocks);
            // Within the room made at the start, so it allocates nothing.
            self.own.resize(end, UNREAD);
            let (first, width, wood) = (self.first, self.width.max(1), &self.wood);
            let blocks = self.own.get_mut(start..end)?;
            band::for_each(runner, blocks, (0, SURVEY_BAND), &|number, band| {
                for (offset, slot) in band.iter_mut().enumerate() {
                    let index = start + number * SURVEY_BAND + offset;
                    let at = |at: usize, first: u32| {
                        first.saturating_add(u32::try_from(at).unwrap_or(u32::MAX))
                    };
                    let block = (at(index % width, first.0), at(index / width, first.1));
                    *slot = wood.own(block, (fields, prototypes));
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

/// A wood far off being matched to the trees stood one by one before it, a
/// run of the places over the ring short of where it begins at a time: the
/// wood, that ring as a wood of its own, how many trees stood in it, the
/// first of its places read and how many columns and rows of them there are,
/// the next to read, one run's reading, and how well the ground suits each
/// tree read so far and the room its crown asks.
#[derive(Debug)]
pub(crate) struct Matching {
    wood: FarWood,
    ring: FarWood,
    stood: usize,
    first: (u32, u32),
    span: (usize, usize),
    next: usize,
    read: Vec<(f64, f64)>,
    grown: Vec<(f64, f64)>,
}

impl Matching {
    /// How far the matching has come, `0.0..=1.0`: its places read, and then
    /// the share of the ground its crowns fill matched to what they grow.
    pub(crate) fn done(&self) -> f64 {
        share(self.next, self.span.0 * self.span.1 + 1)
    }

    /// Read the next run of the ring's places across `runner`, on the land in
    /// `fields`, or once every place is read match the wood to what they
    /// grow: the matching to carry on with, or the wood matched; `None` when
    /// the heap will not hold what they grow.
    pub(crate) fn step(
        mut self,
        fields: &[Heightfield],
        runner: &dyn JobRunner,
    ) -> Option<ControlFlow<FarWood, Self>> {
        let places = self.span.0 * self.span.1;
        if self.next >= places {
            return Some(ControlFlow::Break(self.matched(runner)));
        }
        let (start, end) = (self.next, (self.next + MATCH_UNIT).min(places));
        let (ring, sampling) = (&self.ring, (self.first, self.span.0));
        let read = self.read.get_mut(..end - start)?;
        band::for_each(runner, read, (0, MATCH_BAND), &|number, band| {
            for (offset, slot) in band.iter_mut().enumerate() {
                *slot = ring.read(
                    ring.sample(sampling, start + number * MATCH_BAND + offset),
                    fields,
                );
            }
        });
        let grown = |&&(suits, _): &&(f64, f64)| suits > 0.0;
        let read = self.read.get(..end - start)?;
        self.grown
            .try_reserve(read.iter().filter(grown).count())
            .ok()?;
        self.grown.extend(read.iter().filter(grown));
        self.next = end;
        Some(ControlFlow::Continue(self))
    }

    /// The wood, its crowns filling as much of the ground over the ring as
    /// would stand as many trees there as stood, summed across `runner`: as
    /// it was where none stood there, or none grows.
    fn matched(self, runner: &dyn JobRunner) -> FarWood {
        let Self {
            wood,
            ring,
            stood,
            mut grown,
            ..
        } = self;
        if stood == 0 || grown.is_empty() {
            return wood;
        }
        // Each place read stands for so many of the ring's.
        let wanted = real(stood) / f64::from(SAMPLED * SAMPLED);
        let (mut least, mut most) = (0.0, 4.0);
        for _ in 0..BISECTED {
            let middle = f64::midpoint(least, most);
            let standing = band::fold(
                runner,
                &mut grown,
                (0, MATCH_BAND),
                0.0,
                &|_, band| {
                    band.iter()
                        .map(|&(suits, room)| ring.kept(suits, room, middle).min(1.0))
                        .sum()
                },
                &|sum, more| sum + more,
            );
            if standing < wanted {
                least = middle;
            } else {
                most = middle;
            }
        }
        FarWood {
            filled: f64::midpoint(least, most),
            ..wood
        }
    }
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

/// Where a wood far off stands, and the lattice it is hashed over.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Stretch {
    /// Where it is seen from and the way the eye looks.
    pub(crate) eye: (f64, f64),
    pub(crate) heading: f64,
    /// How far either side of the view it stands, in radians.
    pub(crate) across: f64,
    /// How far off it begins and ends.
    pub(crate) from: f64,
    pub(crate) to: f64,
    /// The cell of its lattice, and how closely the trees stood one by one
    /// before it were sown.
    pub(crate) cell: f64,
    pub(crate) sown: f64,
}

impl FarWood {
    /// `reader`'s trees over `stretch` of the land `grids` trace in
    /// `fields`, its places drawn under `seed`; `None` if it holds no kind or
    /// no ground.
    pub(crate) fn new(
        (reader, seed): (Reader, u32),
        (grids, fields): (Grids, &[Heightfield]),
        stretch: Stretch,
    ) -> Option<Self> {
        let tallest = reader.kinds().map(Habit::tallest).fold(0.0, f64::max);
        let reach = reader
            .kinds()
            .map(|habit| habit.crown * habit.tallest())
            .fold(0.0, f64::max);
        let cell = stretch.cell;
        let usable = |length: f64| length.is_finite() && length > 0.0;
        if tallest <= 0.0 || !usable(cell) || !usable(stretch.sown) || stretch.to <= stretch.from {
            return None;
        }
        let traced = grids.traced(fields);
        let ((cx, cz), half) = traced;
        Some(Self {
            reader,
            grids,
            eye: stretch.eye,
            heading: stretch.heading,
            across: stretch.across,
            facing: (mathf::sin(stretch.heading), mathf::cos(stretch.heading)),
            widest: mathf::cos(stretch.across),
            from: stretch.from,
            to: stretch.to,
            traced,
            origin: (cx - half, cz - half),
            cell,
            spread: spread(reach, cell)?,
            seed,
            sown: stretch.sown,
            filled: PACKED,
            reach,
            tallest,
            rise: 1.0,
            survey: None,
        })
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
        let span = f64::from(BLOCK) * self.cell;
        let block =
            move |at: f64, origin: f64| crate::noise::cell(((at - origin) / span).max(0.0)).0;
        (0..TILES * TILES).filter_map(move |index| {
            let (column, row) = (index % TILES, index / TILES);
            let from = (
                cx - half + f64::from(column) * side,
                cz - half + f64::from(row) * side,
            );
            let to = (from.0 + side, from.1 + side);
            // A crown standing in the ring may reach past its edge.
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
        self.spread = spread(self.reach, self.cell)?;
        Some(())
    }

    /// Its survey begun once its trees are measured as their prototypes in
    /// `prototypes` grew: its lattice to be read a block at a time for the
    /// lowest the ground lies under each, the highest any crown reaching over
    /// it stands, and which of its cells keep a tree. Every place's tree is
    /// read once there, so a ray reads only those it may meet. `None` when
    /// the heap will not hold it.
    pub(crate) fn surveying(mut self, prototypes: &[Prototype]) -> Option<Surveying> {
        self.measure(prototypes)?;
        let span = f64::from(BLOCK) * self.cell;
        let (low, high) = self.extent();
        let index = |at: f64, origin: f64| crate::noise::cell(((at - origin) / span).max(0.0)).0;
        let first = (index(low.0, self.origin.0), index(low.1, self.origin.1));
        let last = (index(high.0, self.origin.0), index(high.1, self.origin.1));
        let across = if low.0 <= high.0 && low.1 <= high.1 {
            (
                last.0.checked_sub(first.0)? + 1,
                last.1.checked_sub(first.1)? + 1,
            )
        } else {
            (0, 0)
        };
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

    /// The corners of the ground its crowns may stand over: its ring across
    /// the view and as far again as a crown reaches, within the traced square.
    /// Empty where the two do not meet.
    fn extent(&self) -> ((f64, f64), (f64, f64)) {
        let (ex, ez) = self.eye;
        let (mut low, mut high) = (
            (f64::INFINITY, f64::INFINITY),
            (f64::NEG_INFINITY, f64::NEG_INFINITY),
        );
        let mut take = |angle: f64, distance: f64| {
            let (x, z) = (
                ex + distance * mathf::sin(angle),
                ez + distance * mathf::cos(angle),
            );
            low = (low.0.min(x), low.1.min(z));
            high = (high.0.max(x), high.1.max(z));
        };
        // The ring's corners, and its far edge's furthest along either axis
        // wherever that lies across the view: its near edge never reaches
        // further than its far one the same way.
        for side in [-1.0, 1.0] {
            for distance in [self.from, self.to] {
                take(self.heading + side * self.across, distance);
            }
        }
        for quarter in 0..4u8 {
            let angle = f64::from(quarter) * core::f64::consts::FRAC_PI_2;
            if wrapped(angle - self.heading).abs() <= self.across {
                take(angle, self.to);
            }
        }
        let ((cx, cz), half) = self.traced;
        (
            (
                (low.0 - self.reach).max(cx - half),
                (low.1 - self.reach).max(cz - half),
            ),
            (
                (high.0 + self.reach).min(cx + half),
                (high.1 + self.reach).min(cz + half),
            ),
        )
    }

    /// Block `(x, z)` as its own cells read, on the land in `fields` with its
    /// trees grown from `prototypes`: the lowest its ground lies, the highest
    /// a tree standing in it rises, and which of its cells keep one, read
    /// only where its ground could root a tree at all.
    fn own(&self, (x, z): (u32, u32), (fields, prototypes): (&[Heightfield], &[Prototype])) -> Own {
        let span = f64::from(BLOCK) * self.cell;
        let from = (
            self.origin.0 + f64::from(x) * span,
            self.origin.1 + f64::from(z) * span,
        );
        let square = (from, (from.0 + span, from.1 + span));
        let (low, high) = self.grids.extremes(fields, square);
        let low = if low.is_finite() {
            low
        } else {
            f64::NEG_INFINITY
        };
        let mut own = Own {
            low: below(low),
            high: f32::NEG_INFINITY,
            marks: [0; WORDS],
        };
        if !self.roots(square, (low, high), fields) {
            return own;
        }
        let mut highest = f64::NEG_INFINITY;
        for bit in 0..BLOCK * BLOCK {
            let cell = (
                x.saturating_mul(BLOCK).saturating_add(bit % BLOCK),
                z.saturating_mul(BLOCK).saturating_add(bit / BLOCK),
            );
            let Some(tree) = self.tree_of(self.place(cell), fields) else {
                continue;
            };
            let Some(grown) = self
                .grown_from(&tree)
                .and_then(|(prototype, _)| prototypes.get(prototype as usize))
            else {
                continue;
            };
            let Some(word) = usize::try_from(bit / u64::BITS)
                .ok()
                .and_then(|word| own.marks.get_mut(word))
            else {
                continue;
            };
            *word |= 1 << (bit % u64::BITS);
            highest = highest.max(tree.base + grown.bounds().max.y * tree.scale);
        }
        own.high = above(highest);
        own
    }

    /// Whether any tree of the wood could root in the square `square`, whose
    /// ground lies between `low` and `high`: in the ring across the view,
    /// between the heights it keeps to, where something grows or the wood
    /// roots bare.
    fn roots(
        &self,
        square: ((f64, f64), (f64, f64)),
        (low, high): (f64, f64),
        fields: &[Heightfield],
    ) -> bool {
        if !self.reaches(square, 0.0) || !high.is_finite() {
            return false;
        }
        let rooting = &self.reader.rooting;
        if rooting.above.is_some_and(|(least, _)| high <= least)
            || rooting.below.is_some_and(|(_, most)| low >= most)
        {
            return false;
        }
        rooting.bare > 0.0 || self.grids.greenest(fields, square) > 0.0
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
        let span = f64::from(BLOCK) * self.cell;
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
    /// hold its trees: whether it overlaps the ring the wood stands in,
    /// across the view.
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
        let furthest = corners
            .iter()
            .map(|&(x, z)| mathf::hypot(x - ex, z - ez))
            .fold(0.0, f64::max);
        if nearest > self.to || furthest < self.from {
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
        self.survey
            .as_ref()
            .map_or(Some(f64::INFINITY), |survey| survey.standing(cell))
    }

    /// Whether a tree at `at` belongs to this wood: off the near wood, within
    /// the ring and across the view, on traced ground.
    fn holds(&self, at: (f64, f64)) -> bool {
        let (dx, dz) = (at.0 - self.eye.0, at.1 - self.eye.1);
        let square = dx * dx + dz * dz;
        let ((cx, cz), half) = self.traced;
        square >= self.from * self.from
            && square <= self.to * self.to
            && (at.0 - cx).abs() < half
            && (at.1 - cz).abs() < half
            && self.faces((dx, dz), square)
    }

    /// Whether the way `(dx, dz)` from the eye, `square` its length squared,
    /// lies across the view: within its half-width of the way it looks.
    fn faces(&self, (dx, dz): (f64, f64), square: f64) -> bool {
        let along = dx * self.facing.0 + dz * self.facing.1;
        let edge = self.widest * self.widest * square;
        if self.widest >= 0.0 {
            along >= 0.0 && along * along >= edge
        } else {
            along >= 0.0 || along * along <= edge
        }
    }

    /// The place cell `cell` holds, and its draws.
    fn place(&self, (column, row): (u32, u32)) -> ((f64, f64), u32) {
        let draw = hash3(column, row, 0xfa2_700d, self.seed);
        (
            (
                self.origin.0 + (f64::from(column) + unit(mix32(draw ^ 0x2c1b_3c6d))) * self.cell,
                self.origin.1 + (f64::from(row) + unit(mix32(draw ^ 0x297a_2d39))) * self.cell,
            ),
            draw,
        )
    }

    /// The tree the place `(at, draw)` holds, if one grows there and keeps
    /// its room, on the land in `fields`.
    fn tree_of(&self, (at, draw): ((f64, f64), u32), fields: &[Heightfield]) -> Option<Tree> {
        if !self.holds(at) {
            return None;
        }
        self.grown(&self.reader.sprout((at, draw), None)?, draw, fields)
    }

    /// The tree `sprout`, drawn as `draw` has it, grows on the land in
    /// `fields`, if its cell keeps one: as many to a cell as the trees stood
    /// one by one come to — places sown as closely as theirs, each growing a
    /// tree as often as the ground suits it, thinned to the room their crowns
    /// ask, which they fill only where they crowd.
    fn grown(&self, sprout: &Sprout, draw: u32, fields: &[Heightfield]) -> Option<Tree> {
        let (tree, suits) = self.reader.grow(sprout, (&self.grids, fields))?;
        let kept = self.kept(suits, tree.apart * tree.reach, self.filled);
        (unit(mix32(draw ^ 0x7f4a_7c15)) < kept).then_some(tree)
    }

    /// How often a cell keeps a tree grown where the ground suits one `suits`
    /// of the way, its crown asking `room` of its neighbours, the wood's
    /// crowns filling `filled` of the ground where they crowd.
    fn kept(&self, suits: f64, room: f64, filled: f64) -> f64 {
        let crowded = filled / (core::f64::consts::PI * room * room).max(1e-9);
        let offered = suits / (self.sown * self.sown);
        self.cell * self.cell * crowded * (1.0 - mathf::exp(-offered / crowded))
    }

    /// Its matching to the trees stood one by one before it, at `stood`,
    /// begun: over the ring short of where it begins, its crowns are to fill
    /// as much of the ground as would stand as many trees there as stood.
    /// `None` when the heap will not hold a run's reading.
    pub(crate) fn matching(self, stood: impl Iterator<Item = (f64, f64)>) -> Option<Matching> {
        let ring = Self {
            from: MATCHED * self.from,
            to: self.from,
            ..self.clone()
        };
        let stood = stood.filter(|&at| ring.holds(at)).count();
        let (first, span) = ring.sampling();
        let read = fallible::filled(MATCH_UNIT, (0.0, 0.0))?;
        Some(Matching {
            wood: self,
            ring,
            stood,
            first,
            span,
            next: 0,
            read,
            grown: Vec::new(),
        })
    }

    /// The first of the places one cell in [`SAMPLED`] each way over the
    /// ground its crowns may stand over, and how many columns and rows of
    /// them there are.
    fn sampling(&self) -> ((u32, u32), (usize, usize)) {
        let (low, high) = self.extent();
        let index =
            |at: f64, origin: f64| crate::noise::cell(((at - origin) / self.cell).max(0.0)).0;
        let first = (index(low.0, self.origin.0), index(low.1, self.origin.1));
        let count = |(from, low, high): (u32, f64, f64), origin: f64| {
            if high < low {
                return 0;
            }
            usize::try_from(index(high, origin).saturating_sub(from) / SAMPLED + 1).unwrap_or(0)
        };
        (
            first,
            (
                count((first.0, low.0, high.0), self.origin.0),
                count((first.1, low.1, high.1), self.origin.1),
            ),
        )
    }

    /// The `index`th of the places [`Self::sampling`] counts from `first`,
    /// `columns` of them a row.
    fn sample(&self, (first, columns): ((u32, u32), usize), index: usize) -> ((f64, f64), u32) {
        let columns = columns.max(1);
        let along = |at: usize, first: u32| {
            first
                .saturating_add(u32::try_from(at).map_or(u32::MAX, |at| at.saturating_mul(SAMPLED)))
        };
        self.place((
            along(index % columns, first.0),
            along(index / columns, first.1),
        ))
    }

    /// How well the ground at the place `place`, on the land in `fields`,
    /// suits the tree it grows there and the room its crown asks of its
    /// neighbours: nought where it grows none of this wood's.
    fn read(&self, place: ((f64, f64), u32), fields: &[Heightfield]) -> (f64, f64) {
        if !self.holds(place.0) {
            return (0.0, 0.0);
        }
        self.reader
            .sprout(place, None)
            .and_then(|sprout| self.reader.grow(&sprout, (&self.grids, fields)))
            .map_or((0.0, 0.0), |(tree, suits)| (suits, tree.apart * tree.reach))
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

    /// The prototype the tree at the place `place` is placed from, as
    /// [`Self::placing_of`] places it.
    fn placed(
        &self,
        place: ((f64, f64), u32),
        fields: &[Heightfield],
    ) -> Option<((u32, Pose, f64, u32), usize)> {
        self.placing_of(&self.tree_of(place, fields)?, place)
    }

    /// Where the tree a hit on cell `cell` met stands, and its key: what its
    /// pattern is fixed in and set apart by.
    pub(crate) fn placing(&self, cell: (u32, u32), fields: &[Heightfield]) -> Option<(Pose, u32)> {
        self.placed(self.place(cell), fields)
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
        let mut walk = Walk::from((self.origin, self.cell), ray, t);
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
                    walk = Walk::from((self.origin, self.cell), ray, t);
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
        let place @ (at, draw) = wood.place(cell);
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
        if lowest > ground + wood.tallest || !wood.holds(at) {
            return;
        }
        let Some(sprout) = wood.reader.sprout(place, None) else {
            return;
        };
        if lowest > ground + wood.rise * wood.reader.highest(&sprout) {
            return;
        }
        let Some(tree) = wood.grown(&sprout, draw, fields) else {
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
    /// How far off it begins.
    pub(crate) const fn begins(&self) -> f64 {
        self.from
    }

    /// Whether `at` lies across its view, however far off.
    pub(crate) fn across(&self, at: (f64, f64)) -> bool {
        let (dx, dz) = (at.0 - self.eye.0, at.1 - self.eye.1);
        wrapped(mathf::atan2(dx, dz) - self.heading).abs() <= self.across
    }

    /// How many trees to a square metre it would stand over the ring from
    /// `from` to `to` across its view on the land in `fields`, its places read
    /// as matching reads them, and the squares of their heights summed to a
    /// square metre, which the ground their crowns cover goes as.
    pub(crate) fn density(&self, (from, to): (f64, f64), fields: &[Heightfield]) -> (f64, f64) {
        let ring = Self {
            from,
            to,
            ..self.clone()
        };
        let (mut within, mut standing, mut covered) = (0u32, 0u32, 0.0);
        let (first, span) = ring.sampling();
        for place in (0..span.0 * span.1).map(|index| ring.sample((first, span.0), index)) {
            if ring.holds(place.0) {
                within += 1;
                if let Some(tree) = ring.tree_of(place, fields) {
                    standing += 1;
                    covered += tree.height * tree.height;
                }
            }
        }
        let area = f64::from(within.max(1)) * self.cell * self.cell;
        (f64::from(standing) / area, covered / area)
    }
}

#[cfg(test)]
#[path = "far_wood_tests.rs"]
mod tests;

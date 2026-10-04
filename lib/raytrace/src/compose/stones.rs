//! Stones a scene sets out: a few rocks of the region's own strewn where the
//! ground takes them, each placed as often as the ground takes it — sized,
//! turned, tilted to the ground it lies on and bedded into it; and a
//! stream's bed, laid stone by stone as its water left it.
//!
//! A bed's stones are as big as the floods that laid them could move: the
//! median is the stone the stream's bankfull flow just stirs (Shields'
//! criterion), the rest spread lognormally about it. Every stretch of the
//! stream above feeds the bed alike, so each stone has come a distance drawn
//! evenly from nought to as far as the water has run, and is worn as far as
//! it came: fresh and angular, or round. Each lies on its flattest side, its
//! longest across the stream and its upstream edge tucked under the stone
//! before it, imbricated as moving water leaves stones, and bedded into the
//! gravel by a share of its height. The stones are drawn from lattices about
//! the eye a size class apiece, each cell holding one or none, so the bed is
//! the same however its work is divided; laid largest first, each where no
//! stone already laid lies; and kept to those spanning a few pixels at their
//! distance, so the bed is laid as finely as it is seen, the gravel finer
//! than that the ground's own.

use alloc::vec::Vec;
use core::f64::consts::{FRAC_PI_2, PI, TAU};

use tairix_parallel::JobRunner;
use tairix_rng::NonCryptoRng;
use tairix_util::{fallible, mathf};

use super::chains::Chains;
use super::plants::{Drift, Piece};
use super::woodland::{Ranking, Runs, KEPT};
use super::{rgb, Dice, Recipe, Stage, GRANITES};
use crate::band;
use crate::channel::{Banked, Form, Section, Station, BROADEST};
use crate::deadwood::{LOG_TAPER, SUNK};
use crate::ground::Rock;
use crate::heightfield::Heightfield;
use crate::land::{seam, Land};
use crate::material::{Finish, Material, Relief, CARRIED_FOAM};
use crate::noise::hash3;
use crate::noise::smoothstep;
use crate::pigment::Pigment;
use crate::rock::{Habit, Lithology, MOST_FRACTURES};
use crate::sample::{mix32, unit};
use crate::shape::Shape;
use crate::stream::{Flow, Stone, Stretch};
use crate::vector::{byte, real, share, single, Frame, Pose, Vec3};

/// How many rocks a scene grows to strew, choosing among them.
const KINDS: usize = 4;

/// A scene's rocks, planned.
#[derive(Copy, Clone, Debug)]
pub(super) struct Stones {
    kinds: [(u32, f64); KINDS],
    material: usize,
}

impl Stones {
    /// Rocks of `rock`, planned to be grown before the scene is traced;
    /// `None` when the heap will not hold them.
    pub(super) fn new(stage: &mut Stage, dice: &mut Dice, rock: Rock) -> Option<Self> {
        let material = stage.material(
            Material::new(
                Pigment::Rock(Rock {
                    seed: dice.seed(),
                    ..rock
                }),
                Finish::Coated { roughness: 0.82 },
            )
            .with_relief(grain(dice)),
        )?;
        let mut kinds = [(0, 0.0); KINDS];
        for kind in &mut kinds {
            let habit = Habit {
                squash: dice.range(0.45, 0.85),
                elongation: dice.range(0.7, 1.0),
                fractures: dice.count(1, MOST_FRACTURES),
                cleaved: false,
            };
            // Weathered where it lies, a little, never carried.
            let recipe = Recipe::Rock {
                habit,
                wear: dice.range(0.0, 0.3),
                seed: dice.wide(),
            };
            *kind = (stage.plan(&recipe)?, habit.squash);
        }
        Some(Self { kinds, material })
    }

    /// A stone `size` across lying on the ground at `base`, which faces
    /// `normal` there: tilted part way to it, turned any way, and a third of
    /// its height bedded in.
    pub(super) fn lay(
        &self,
        stage: &mut Stage,
        dice: &mut Dice,
        (base, normal): (Vec3, Vec3),
        size: f64,
    ) -> Option<()> {
        let (prototype, squash) = dice.pick(&self.kinds)?;
        let scale = 0.5 * size;
        let upright = Vec3::UP.lerp(normal, 0.6).normalized();
        let frame = Frame::turned(dice.range(0.0, TAU), dice.range(-0.15, 0.15))
            .aligning(Vec3::UP, upright);
        let pose = Pose::new(base + upright * bedded(squash * scale, 1.0 / 3.0), frame);
        stage.add(
            Shape::Instance {
                prototype,
                pose,
                scale,
                key: dice.seed(),
            },
            self.material,
            pose,
            false,
        )?;
        Some(())
    }
}

/// Where a boulder `size` across let go at `at` on `land`, whose grids are
/// `fields`, comes to rest: rolled down whatever is steeper, over its own
/// breadth, than a boulder rests on, until the ground holds it; `None` if it
/// rolls too far to follow.
fn rolled(
    (land, fields): (&Land, &[Heightfield]),
    at: (f64, f64),
    size: f64,
) -> Option<(f64, f64)> {
    let reach = (0.5 * size).max(ROLL);
    let mut at = at;
    for _ in 0..ROLLS {
        let height = |dx: f64, dz: f64| land.height(fields, at.0 + dx, at.1 + dz);
        let (dx, dz) = (
            height(reach, 0.0) - height(-reach, 0.0),
            height(0.0, reach) - height(0.0, -reach),
        );
        let grade = mathf::hypot(dx, dz);
        if grade <= 2.0 * reach * BOULDER_REST {
            return Some(at);
        }
        at = (at.0 - dx / grade * ROLL, at.1 - dz / grade * ROLL);
    }
    None
}

/// How far above the ground a stone's middle stands when it is `half` high
/// either way of it and `buried` of its whole height lies in the ground.
fn bedded(half: f64, buried: f64) -> f64 {
    half * (1.0 - 2.0 * buried)
}

/// A stone's surface grain, a few centimetres across a metre.
fn grain(dice: &mut Dice) -> Relief {
    Relief::Grain {
        depth: 0.29,
        scale: 14.0,
        seed: dice.seed(),
    }
}

/// The stretch of stream a scene's eye looks along: which of the land's
/// rivers, where along it the eye stands, what shapes its channel, how far
/// its water has run to come there, and the rock its bed is of.
#[derive(Copy, Clone, Debug)]
pub(super) struct Brook {
    pub(super) course: usize,
    pub(super) station: Station,
    pub(super) form: Form,
    pub(super) run: f64,
    pub(super) lithology: Lithology,
}

/// The size classes a bed's stones are drawn in, each twice the last; the
/// least stone the bed lays and the most it ever does.
const CLASSES: usize = 8;
const LEAST: f64 = 0.012;
const MOST: f64 = 1.2;

/// How many shapes a bed's rock breaks to, and at how many wears each is
/// grown, from fresh to as worn as the farthest-carried.
const SHAPES: usize = 4;
const WEARS: usize = 4;

/// The share of a bed its stones cover, seen from above; and how many fewer
/// lie on its sand, and on a ledge's bare rock.
const COVER: f64 = 0.85;
const ON_SAND: f64 = 0.6;
const ON_ROCK: f64 = 0.8;

/// How far apart the sections of a stretch's channel are read for its flow.
const SECTION_SPACING: f64 = 0.1;

/// The gentlest a bed's floods are taken to fall when sizing its stones.
const LEAST_FALL: f64 = 1e-3;

/// The largest median a bed's stones take: past it a stream's floods move
/// boulders, which its banks and its own steps hold back.
const MEDIAN: f64 = 0.25;

/// Rows of a lattice a core reads in a unit, stones thinned in one, and
/// stones set out in one.
const ROWS: usize = 8;
const THIN: usize = 4096;
const PLACED: usize = 8192;

/// How much of a stone's breadth, seen from above, no other stone overlaps:
/// stones nest into the gaps between their neighbours and lie partly under
/// them.
const HARD: f64 = 0.8;

/// How far from the eye a stone behind its shoulder may still lie: about it
/// it shows in the water's reflection and casts its shadow in the view.
const ABOUT: f64 = 3.0;
/// How far off the view either way a stone further off is laid.
const VIEW: f64 = 1.4;

/// The breadth of the grid stones are kept apart over, and the most cells it
/// spans a side.
const KEPT_APART: f64 = 0.2;
const MOST_SIDE: usize = 1024;

/// The lattice a bed's boulders are drawn from, after its size classes'; a
/// cell's breadth; and the least and the most boulder, and outcrop.
const BOULDERS: usize = CLASSES;
const LATTICES: usize = CLASSES + 1;
const BOULDER_CELL: f64 = 1.6;
const BOULDER: (f64, f64) = (0.35, 1.3);
const OUTCROP: (f64, f64) = (0.6, 1.6);
/// How likely a cell at a bank's foot holds a boulder fallen from it, where
/// the bank is cut or its rock outcrops and where it is neither; and a cell
/// out in the channel.
const AT_THE_FOOT: (f64, f64) = (0.7, 0.2);
const ADRIFT: f64 = 0.04;
/// How much of a boulder's faces turned to the sky moss mantles, and the
/// moss's colour.
const MOSS: (f64, u32) = (0.7, 0x4A_58_26);

/// The steepest ground, as rise over run, gravel rests on and a boulder on
/// soil does; how far a boulder rolls a step, and the most steps it rolls
/// before it is taken to have rolled out of reach.
const GRAVEL_REST: f64 = 0.7;
const BOULDER_REST: f64 = 0.5;
const ROLL: f64 = 0.1;
const ROLLS: u32 = 60;

/// How many pieces a stream's floods leave lodged in the stretch the eye
/// looks over, the least and the most, and how they lie, drawn evenly from
/// these; how likely a sunk piece, and one fallen in from the bank, is a
/// trunk rather than a branch; how likely a trunk lies across the stream as
/// well, where it is narrow enough for one to, and the current has jammed
/// branches against it, and how many; and how far along the stream about
/// the eye the drift is laid.
const PIECES: (u32, u32) = (7, 14);
const LODGINGS: [Lodged; 6] = [
    Lodged::Stranded,
    Lodged::Jammed,
    Lodged::Jammed,
    Lodged::Sunk,
    Lodged::Sunk,
    Lodged::Leaning,
];
const SUNK_TRUNKS: f64 = 0.4;
const LEANING_TRUNKS: f64 = 0.5;
const TRUNK: f64 = 0.35;
const NARROW: f64 = 6.0;
const JAMMED: f64 = 0.6;
const JAM: (u32, u32) = (3, 6);
const DRIFT_REACH: (f64, f64) = (-8.0, 30.0);

/// How a piece of drift lies in a stream.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Lodged {
    /// Stranded at the water's edge up a bar, half in the water.
    Stranded,
    /// Jammed across the flow, its ends on the bed.
    Jammed,
    /// Waterlogged and sunk, lying along the current on the bed.
    Sunk,
    /// Fallen in from the bank, its foot there and its top in the water.
    Leaning,
    /// A trunk undercut from one bank, lying across the stream to the other.
    Spanning,
}

/// A piece of drift lain: where its foot lies, which way it runs from
/// there, and how long and thick it is.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Lain {
    start: (f64, f64),
    heading: f64,
    length: f64,
    radius: f64,
}

/// What came of laying a piece of drift: where it lay, or that it found no
/// room.
#[derive(Copy, Clone, Debug, PartialEq)]
enum Lay {
    Lain(Lain),
    Crowded,
}
/// How many places a piece of drift is tried at, and the key its draws are
/// made under.
const DRIFT_TRIES: u32 = 6;
const DRIFT_KEY: usize = 0x0d1f;

/// A stream's bed, set out about the eye a unit at a time once its land
/// stands.
#[derive(Debug)]
pub(super) struct Bed {
    brook: Brook,
    eye: (f64, f64),
    heading: f64,
    /// Each wear's prototypes, from the freshest, a shape apiece, and their
    /// habits; the wears they were grown at.
    kinds: [[(u32, Habit); SHAPES]; WEARS],
    wears: [f64; WEARS],
    /// The materials a stone is wet in, under the water, and dry in, and a
    /// boulder mossed in.
    wet: usize,
    dry: usize,
    mossy: usize,
    sizes: Sizes,
    /// What the floods left lodged in it, if the land grows any.
    drift: Option<Drift>,
    /// How far about the eye it is laid, the most stones, the angle a pixel
    /// spans and the fewest it lays a stone across.
    reach: f64,
    most: u32,
    pixel: f64,
    pixels: f64,
    seed: u32,
    found: Runs<Found>,
    ranking: Ranking,
    chains: Option<Chains>,
    laid: Vec<Laid>,
    /// The stones the water runs over, for its flow to be solved.
    stones: Vec<Stone>,
    pass: Pass,
}

/// How far a bed is laid.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Pass {
    Reading { class: usize, row: usize },
    Ranking,
    Thinning,
    Placing { next: usize },
    Lodging,
    Done,
}

/// A bed's stones by size, as each size class's chance a cell of its
/// lattice holds a stone.
#[derive(Copy, Clone, Debug)]
struct Sizes {
    chances: [f64; CLASSES],
}

/// A stone a lattice's cell holds: where, how big, its key, and how it came
/// to lie there.
#[derive(Copy, Clone, Debug)]
struct Found {
    at: (f64, f64),
    size: f64,
    key: u32,
    lying: Lying,
}

/// How a stone came to lie where it does.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Lying {
    /// Laid by the water among the rest of its bed.
    Bedded,
    /// Fallen from a bank and rolled to where the ground holds it.
    Fallen,
    /// A block of a bank's own rock standing out of its face.
    Outcropping,
}

/// A stone laid: where, how far no other may come, its prototype's habit
/// and index, its wear's index, its key, and how it lies.
#[derive(Copy, Clone, Debug)]
struct Laid {
    at: (f64, f64),
    size: f64,
    hard: f64,
    shape: usize,
    wear: usize,
    key: u32,
    lying: Lying,
}

impl Sizes {
    /// The stones of a bed whose bankfull flow at `station` just stirs its
    /// median, spread about it as `sorting`, the standard deviation of their
    /// sizes' base-two logarithm, has it.
    fn of(station: &Station, sorting: f64) -> Self {
        // Shields' criterion, a critical stress of 0.045 over stone 2.65
        // times as dense as water.
        let slope = station.fall.max(LEAST_FALL);
        let median = (station.depth * slope / (1.65 * 0.045)).clamp(0.02, MEDIAN);
        let mut chances = [0.0; CLASSES];
        let centre = mathf::ln(median) / core::f64::consts::LN_2;
        for (class, chance) in chances.iter_mut().enumerate() {
            let least = Self::least(class);
            if least >= MOST {
                break;
            }
            let at =
                |size: f64| normal((mathf::ln(size) / core::f64::consts::LN_2 - centre) / sorting);
            let area = at(2.0 * least) - at(least);
            // A stone's mean square size within a class twice its least, as
            // its sizes spread evenly in their logarithm, and its outline's
            // share of its square at a typical elongation.
            let typical = least * least * 3.0 / (2.0 * core::f64::consts::LN_2) * 0.62;
            let cell = 2.0 * least;
            *chance = (COVER * area / typical * cell * cell).min(1.0);
        }
        Self { chances }
    }

    /// The least stone of class `class`.
    fn least(class: usize) -> f64 {
        LEAST * real(1 << class)
    }
}

/// The standard normal distribution's share below `x` (Abramowitz and
/// Stegun 7.1.26, to within about 1.5e-7).
fn normal(x: f64) -> f64 {
    let z = x.abs() / core::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.327_591_1 * z);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let erf = 1.0 - poly * mathf::exp(-z * z);
    if x >= 0.0 {
        f64::midpoint(1.0, erf)
    } else {
        f64::midpoint(1.0, -erf)
    }
}

/// A square lattice of one size class's cells about the eye: its first
/// cell's corner, its cells a side, and their breadth.
#[derive(Copy, Clone, Debug)]
struct Lattice {
    corner: (f64, f64),
    side: usize,
    cell: f64,
}

impl Lattice {
    /// Cell `(column, row)`'s key for size class `class` under `seed`,
    /// drawn from its place on the land's own grid, and where in the cell its
    /// stone would stand.
    fn draw(
        &self,
        (column, row): (usize, usize),
        class: usize,
        seed: u32,
    ) -> Option<(u32, (f64, f64))> {
        let place = |value: f64| mathf::round_i32(mathf::floor(value / self.cell)).cast_unsigned();
        let (x0, z0) = (
            self.corner.0 + real(column) * self.cell,
            self.corner.1 + real(row) * self.cell,
        );
        let key = hash3(
            place(x0 + 0.5 * self.cell),
            place(z0 + 0.5 * self.cell),
            u32::try_from(class).ok()?,
            seed,
        );
        let at = (
            x0 + self.cell * unit(mix32(key ^ 1)),
            z0 + self.cell * unit(mix32(key ^ 2)),
        );
        Some((key, at))
    }
}

/// Ask `stage` for the bed of `brook` about `vantage` — the eye's place and
/// the way it looks — laid once its land stands, and `drift` lodged in it.
/// One draw of `dice` keys it. `None` when the stage will not hold its
/// prototypes or materials.
pub(super) fn bed(
    stage: &mut Stage,
    dice: &mut Dice,
    (brook, eye, heading): (Brook, (f64, f64), f64),
    drift: Option<Drift>,
) -> Option<()> {
    let mut dice = Dice::keyed(dice.wide(), 0);
    let (wet, dry, mossy) = materials(stage, &mut dice, brook.lithology)?;
    let farthest = (brook.run / brook.lithology.rounding()).min(brook.lithology.most_wear());
    let mut wears = [0.0; WEARS];
    for (index, wear) in wears.iter_mut().enumerate() {
        *wear = farthest * share(index, WEARS - 1);
    }
    let mut kinds = [[(
        0,
        Habit {
            squash: 0.5,
            elongation: 0.8,
            fractures: 0,
            cleaved: false,
        },
    ); SHAPES]; WEARS];
    for shape in 0..SHAPES {
        let mut draws = NonCryptoRng::seed_from_u64(dice.wide());
        let habit = brook.lithology.habit(&mut draws);
        let seed = dice.wide();
        for (wear, row) in wears.iter().zip(kinds.iter_mut()) {
            let prototype = stage.plan(&Recipe::Rock {
                habit,
                wear: *wear,
                seed,
            })?;
            *row.get_mut(shape)? = (prototype, habit);
        }
    }
    let sorting = dice.range(1.0, 1.5);
    let densities = &stage.densities.bed;
    stage.bed = Some(Bed {
        brook,
        eye,
        heading,
        kinds,
        wears,
        wet,
        dry,
        mossy,
        sizes: Sizes::of(&brook.station, sorting),
        drift,
        reach: densities.reach,
        most: densities.most,
        pixel: stage.pixel,
        pixels: densities.pixels,
        seed: dice.seed(),
        found: Runs::default(),
        ranking: Ranking::default(),
        chains: None,
        laid: Vec::new(),
        stones: Vec::new(),
        pass: Pass::Reading { class: 0, row: 0 },
    });
    Some(())
}

/// The materials stones of `lithology` are made in, wet, dry and mossed;
/// `None` when the stage will not hold them.
fn materials(
    stage: &mut Stage,
    dice: &mut Dice,
    lithology: Lithology,
) -> Option<(usize, usize, usize)> {
    // Each rock's two shades its stones fall between, the flecks of its
    // grains and how many to a metre: granite's crystals a few millimetres,
    // a sandstone's or a slate's grains finer than the eye picks out.
    let (bases, flecks, scale) = match lithology {
        Lithology::Granite => {
            let (base, a, b) = dice.pick(&GRANITES)?;
            ([base, 0x8E_86_7E], [a, b], 220.0)
        }
        Lithology::Sandstone => ([0xB0_90_6E, 0x96_76_56], [0x80_62_46, 0xC8_B4_96], 500.0),
        Lithology::Limestone => ([0xC2_BC_B0, 0xA6_A2_96], [0x8E_88_7E, 0xD4_D0_C8], 340.0),
        Lithology::Slate => ([0x5E_64_6C, 0x4C_50_58], [0x3A_3E_46, 0x70_76_7C], 600.0),
    };
    let seed = dice.seed();
    let pebbles = |moss: (f64, Vec3)| Pigment::Pebbles {
        bases: bases.map(rgb),
        flecks: flecks.map(rgb),
        scale,
        shade: 0.18,
        moss,
        seed,
    };
    let pigment = pebbles((0.0, Vec3::ZERO));
    let relief = grain(dice);
    // Wet, a stone darkens and its film of water shines.
    let wet = stage.material(
        Material::new(pigment.clone(), Finish::Coated { roughness: 0.25 })
            .with_relief(relief.clone()),
    )?;
    let dry = stage.material(
        Material::new(pigment.clone(), Finish::Coated { roughness: 0.78 })
            .with_relief(relief.clone()),
    )?;
    let mossed = pebbles((MOSS.0, rgb(MOSS.1)));
    let mossy = stage
        .material(Material::new(mossed, Finish::Coated { roughness: 0.9 }).with_relief(relief))?;
    Some((wet, dry, mossy))
}

impl Bed {
    /// Whether it is all set out.
    pub(super) fn finished(&self) -> bool {
        self.pass == Pass::Done
    }

    /// Which of the land's rivers it is the bed of.
    pub(super) const fn course(&self) -> usize {
        self.brook.course
    }

    /// How far it is set out: its lattices read, most of it, then its
    /// stones ranked, thinned and set out.
    pub(super) fn done(&self) -> f64 {
        let rows = |class: usize| self.lattice(class).map_or(0, |lattice| lattice.side);
        let total: usize = (0..LATTICES).map(rows).sum();
        let read = |class: usize, row: usize| (0..class).map(rows).sum::<usize>() + row;
        match self.pass {
            Pass::Reading { class, row } => 0.7 * share(read(class, row), total),
            Pass::Ranking => 0.7,
            Pass::Thinning => 0.75 + 0.1 * share(self.laid.len(), self.found.len()),
            Pass::Placing { next } => 0.85 + 0.14 * share(next, self.laid.len()),
            Pass::Lodging => 0.99,
            Pass::Done => 1.0,
        }
    }

    /// The stones the water runs over, taken for its flow to be solved, and
    /// the stretch of `land`'s stream they lie in, `behind` and `ahead` of
    /// the eye along it, its channel read every `SECTION_SPACING`: `None`
    /// before the bed is laid, or when the heap will not hold the stretch.
    pub(super) fn take(
        &mut self,
        land: &Land,
        (behind, ahead): (f64, f64),
    ) -> Option<(Stretch, Vec<Stone>)> {
        if self.pass != Pass::Done {
            return None;
        }
        // Ahead of the eye is upstream when it looks up the stream.
        let (behind, ahead) = if self.ahead(land)? >= 0.0 {
            (behind, ahead)
        } else {
            (ahead, behind)
        };
        let from = self.brook.station.along - behind;
        let length = behind + ahead;
        let count =
            usize::try_from(mathf::round_i32(mathf::ceil(length / SECTION_SPACING))).ok()? + 1;
        let mut sections = Vec::new();
        sections.try_reserve(count).ok()?;
        let mut widest = 0.0f64;
        for index in 0..count {
            let along = from + real(index) * SECTION_SPACING;
            let station = Station::on(&land.rivers, self.brook.course, along)?;
            widest = widest.max(station.width);
            sections.push(Section::new(&station, &self.brook.form));
        }
        Some((
            Stretch {
                from,
                length,
                half: 0.5 * widest + 0.5,
                spacing: SECTION_SPACING,
                sections,
            },
            core::mem::take(&mut self.stones),
        ))
    }

    /// Set out the next unit of the bed on `land` across `runner`; `None`
    /// when the heap will not hold it.
    pub(super) fn step(
        &mut self,
        stage: &mut Stage,
        (land, runner): (&Land, &dyn JobRunner),
    ) -> Option<()> {
        match self.pass {
            Pass::Reading { class, row } => self.read((land, &stage.fields), runner, (class, row)),
            Pass::Ranking => {
                self.ranking.rank(runner)?;
                if self.ranking.ranked() {
                    self.chains = Some(Chains::new(
                        (self.eye, self.reach + 1.0),
                        KEPT_APART,
                        MOST_SIDE,
                    )?);
                    self.pass = Pass::Thinning;
                }
                Some(())
            }
            Pass::Thinning => self.thin(),
            Pass::Placing { next } => self.place(stage, land, next),
            Pass::Lodging => {
                self.lodge(stage, land)?;
                self.pass = Pass::Done;
                Some(())
            }
            Pass::Done => Some(()),
        }
    }

    /// The lattice of size class `class`, as far about the eye as its
    /// largest stones still span the pixels the bed lays a stone across;
    /// `None` past the classes the bed lays.
    fn lattice(&self, class: usize) -> Option<Lattice> {
        if class == BOULDERS {
            let snap = |value: f64| mathf::floor(value / BOULDER_CELL) * BOULDER_CELL;
            let side =
                usize::try_from(mathf::round_i32(mathf::ceil(2.0 * self.reach / BOULDER_CELL)) + 1)
                    .ok()?;
            return Some(Lattice {
                corner: (snap(self.eye.0 - self.reach), snap(self.eye.1 - self.reach)),
                side,
                cell: BOULDER_CELL,
            });
        }
        let least = Sizes::least(class);
        if class >= CLASSES || least >= MOST || self.sizes.chances.get(class)? <= &0.0 {
            return None;
        }
        let cell = 2.0 * least;
        let seen = (2.0 * least / (self.pixels * self.pixel)).min(self.reach);
        let snap = |value: f64| mathf::floor(value / cell) * cell;
        let side = usize::try_from(mathf::round_i32(mathf::ceil(2.0 * seen / cell)) + 1).ok()?;
        Some(Lattice {
            corner: (snap(self.eye.0 - seen), snap(self.eye.1 - seen)),
            side,
            cell,
        })
    }

    /// Read the next unit of class `class`'s lattice from row `row` on
    /// `land`, whose grids are `fields`, gathering the stones its cells hold.
    fn read(
        &mut self,
        (land, fields): (&Land, &[Heightfield]),
        runner: &dyn JobRunner,
        (class, row): (usize, usize),
    ) -> Option<()> {
        let Some(lattice) = self.lattice(class) else {
            self.pass = if class + 1 < LATTICES {
                Pass::Reading {
                    class: class + 1,
                    row: 0,
                }
            } else {
                Pass::Ranking
            };
            return Some(());
        };
        let end = (row + ROWS * runner.width().max(1)).min(lattice.side);
        let mut cells = fallible::filled((end - row) * lattice.side, None)?;
        band::for_each(runner, &mut cells, (row, lattice.side), &|row, cells| {
            for (column, cell) in cells.iter_mut().enumerate() {
                *cell = self.holds((land, fields), (&lattice, class), (column, row));
            }
        });
        let count = cells.iter().flatten().count();
        if !self.found.reserve(count) || !self.ranking.reserve(count) {
            return None;
        }
        for found in cells.into_iter().flatten() {
            let index = u32::try_from(self.found.len()).ok()?;
            self.ranking.add(found.size, index);
            self.found.push(found);
        }
        self.pass = if end < lattice.side {
            Pass::Reading { class, row: end }
        } else if class + 1 < LATTICES {
            Pass::Reading {
                class: class + 1,
                row: 0,
            }
        } else {
            Pass::Ranking
        };
        Some(())
    }

    /// The stone cell `(column, row)` of `lattice`, class `class`'s, holds on
    /// `land`, whose grids are `fields`, if any: one of the class's sizes in
    /// its channel, as likely as the bed's stones fall in that class, fewer
    /// on its sand and on bare rock, and only where it would span enough
    /// pixels to be seen.
    fn holds(
        &self,
        (land, fields): (&Land, &[Heightfield]),
        (lattice, class): (&Lattice, usize),
        (column, row): (usize, usize),
    ) -> Option<Found> {
        if class == BOULDERS {
            return self.boulder((land, fields), lattice, (column, row));
        }
        let (key, at) = lattice.draw((column, row), class, self.seed)?;
        if unit(key) >= *self.sizes.chances.get(class)? {
            return None;
        }
        let size = Sizes::least(class) * mathf::exp(core::f64::consts::LN_2 * unit(mix32(key ^ 3)));
        if !self.seen(at, size) {
            return None;
        }
        let near = land.rivers.nearest(at.0, at.1)?;
        if near.distance > 0.5 * BROADEST * near.width + 0.5 * size {
            return None;
        }
        let lie = land.lie(fields, at.0, at.1);
        // Gravel lies no steeper than it rests at; what fell on a steeper
        // wall rolled down to the stones below.
        if lie.upright * lie.upright * (1.0 + GRAVEL_REST * GRAVEL_REST) < 1.0 {
            return None;
        }
        let kept = (1.0 - ON_SAND * smoothstep(0.15, 0.6, lie.sediment))
            * (1.0 - ON_ROCK * smoothstep(-0.3, -0.8, lie.sediment));
        if unit(mix32(key ^ 0xa)) >= kept {
            return None;
        }
        Some(Found {
            at,
            size,
            key,
            lying: Lying::Bedded,
        })
    }

    /// Whether a stone `size` across at `at` would be seen: within the
    /// bed's reach, spanning the pixels it lays a stone across, and in the
    /// view or close enough about the eye to show in its reflection.
    fn seen(&self, at: (f64, f64), size: f64) -> bool {
        let (dx, dz) = (at.0 - self.eye.0, at.1 - self.eye.1);
        let distance = mathf::hypot(dx, dz);
        if distance > self.reach || size < self.pixels * self.pixel * distance {
            return false;
        }
        let off = mathf::atan2(dx, dz) - self.heading;
        let off = off - TAU * mathf::floor((off + core::f64::consts::PI) / TAU);
        distance <= ABOUT || off.abs() <= VIEW
    }

    /// The boulder cell `(column, row)` of the boulders' `lattice` holds on
    /// `land`, whose grids are `fields`, if any: on a bank's face where its
    /// rock outcrops, a block of that rock standing out of it; else one
    /// fallen from a bank, likelier where a pool cuts the bank or its rock
    /// outcrops and larger where it does, rolled down to where the ground
    /// holds it, often into the water at the bank's foot; or now and then
    /// one out in the channel.
    fn boulder(
        &self,
        (land, fields): (&Land, &[Heightfield]),
        lattice: &Lattice,
        (column, row): (usize, usize),
    ) -> Option<Found> {
        let (key, at) = lattice.draw((column, row), BOULDERS, self.seed)?;
        let near = land.rivers.nearest(at.0, at.1)?;
        let banked = Banked::new(&Station::of(&near), &self.brook.form);
        let across = near.side * near.distance;
        let bank = banked.banks.get(usize::from(across >= 0.0))?;
        let beyond = near.distance - banked.section.half;
        if (0.0..bank.face).contains(&beyond) && unit(mix32(key ^ 4)) < bank.rock {
            let size =
                OUTCROP.0 * mathf::exp(mathf::ln(OUTCROP.1 / OUTCROP.0) * unit(mix32(key ^ 3)));
            return self.seen(at, size).then_some(Found {
                at,
                size,
                key,
                lying: Lying::Outcropping,
            });
        }
        let shed = bank.cut.max(bank.rock);
        let chance = if (-0.9..0.5 * bank.face).contains(&beyond) {
            AT_THE_FOOT.1 + (AT_THE_FOOT.0 - AT_THE_FOOT.1) * shed
        } else if beyond < 0.0 {
            ADRIFT
        } else {
            0.0
        };
        if unit(key) >= chance {
            return None;
        }
        let grown = mathf::exp(
            mathf::ln(BOULDER.1 / BOULDER.0) * unit(mix32(key ^ 3)) * (0.6 + 0.4 * bank.rock),
        );
        let size = BOULDER.0 * grown;
        let at = rolled((land, fields), at, size)?;
        self.seen(at, size).then_some(Found {
            at,
            size,
            key,
            lying: Lying::Fallen,
        })
    }

    /// Thin the next unit of the stones found, largest first: each laid where
    /// it keeps clear of every stone laid before it, until the bed holds as
    /// many as it may.
    fn thin(&mut self) -> Option<()> {
        let most = usize::try_from(self.most).unwrap_or(usize::MAX);
        for _ in 0..THIN {
            if self.laid.len() >= most || self.ranking.exhausted() {
                self.chains = None;
                self.pass = Pass::Placing { next: 0 };
                return Some(());
            }
            let Some(index) = self.ranking.next() else {
                continue;
            };
            let found = *self.found.get(index as usize)?;
            let shape = usize::try_from(mix32(found.key ^ 4)).unwrap_or(0) % SHAPES;
            let habit = self.kinds.first()?.get(shape)?.1;
            let hard = HARD * 0.5 * found.size * mathf::sqrt(habit.elongation);
            let chains = self.chains.as_mut()?;
            let (cells, _) = chains.span(found.at, hard);
            let laid = &self.laid;
            let crowded = chains.within(cells).any(|id| {
                laid.get(id as usize).is_some_and(|other| {
                    let (dx, dz) = (found.at.0 - other.at.0, found.at.1 - other.at.1);
                    let apart = hard + other.hard;
                    dx * dx + dz * dz < apart * apart
                })
            });
            if crowded {
                continue;
            }
            let id = u32::try_from(self.laid.len()).ok()?;
            chains.link(id, cells)?;
            // A boulder fell from the bank it lies by, and an outcrop never
            // moved: neither was carried.
            let carried = if found.lying == Lying::Bedded {
                unit(mix32(found.key ^ 5)) * self.brook.run
            } else {
                0.0
            };
            let wear =
                (carried / self.brook.lithology.rounding()).min(self.brook.lithology.most_wear());
            self.laid.try_reserve(1).ok()?;
            self.laid.push(Laid {
                at: found.at,
                size: found.size,
                hard,
                shape,
                wear: self.wear_of(wear),
                key: found.key,
                lying: found.lying,
            });
        }
        Some(())
    }

    /// The index of the wear, among those its stones are grown at, nearest
    /// `wear`.
    fn wear_of(&self, wear: f64) -> usize {
        let farthest = self.wears.last().copied().unwrap_or(0.0);
        if farthest <= 0.0 {
            return 0;
        }
        let at = mathf::round(wear / farthest * real(WEARS - 1));
        usize::try_from(mathf::round_i32(at.clamp(0.0, real(WEARS - 1)))).unwrap_or(0)
    }

    /// Set out the next unit of the stones laid from `next`, on `land`, as far
    /// as the stage has room for them.
    fn place(&mut self, stage: &mut Stage, land: &Land, next: usize) -> Option<()> {
        let end = (next + PLACED).min(self.laid.len());
        for index in next..end {
            if stage.room() <= KEPT {
                self.placed();
                return Some(());
            }
            let stone = *self.laid.get(index)?;
            self.set_out(stage, land, stone)?;
        }
        if end < self.laid.len() {
            self.pass = Pass::Placing { next: end };
        } else {
            self.placed();
        }
        Some(())
    }

    /// Lodge the drift the floods left in the stretch the eye looks over on
    /// `land`, most of it in the water — jammed across its flow, sunk along
    /// its bed, fallen in from its banks — some stranded at its edge; and,
    /// where the stream runs narrow, now and then a trunk undercut from its
    /// bank lying across it, the branches the current brings piled against
    /// it. `None` when the stage will not hold it.
    fn lodge(&mut self, stage: &mut Stage, land: &Land) -> Option<()> {
        let Some(drift) = self.drift else {
            return Some(());
        };
        let mut dice = Dice::keyed(u64::from(self.seed), DRIFT_KEY);
        let ahead = self.ahead(land)?;
        for _ in 0..dice.count(PIECES.0, PIECES.1) {
            let lodged = dice.pick(&LODGINGS)?;
            self.lodge_near(stage, land, &drift, (&mut dice, ahead, lodged))?;
        }
        if self.brook.station.width < NARROW && dice.chance(TRUNK) {
            let spanning = (&mut dice, ahead, Lodged::Spanning);
            if let Lay::Lain(trunk) = self.lodge_near(stage, land, &drift, spanning)? {
                if dice.chance(JAMMED) {
                    self.jam(stage, land, &drift, (&mut dice, trunk))?;
                }
            }
        }
        Some(())
    }

    /// Which way along the brook's course the eye looks: `1.0` down it,
    /// `-1.0` up it.
    fn ahead(&self, land: &Land) -> Option<f64> {
        let (course, along) = (self.brook.course, self.brook.station.along);
        let (before, after) = (
            land.rivers.at(course, along - 1.0)?,
            land.rivers.at(course, along + 1.0)?,
        );
        let looking = (mathf::sin(self.heading), mathf::cos(self.heading));
        let down = looking.0 * (after.x - before.x) + looking.1 * (after.z - before.z);
        Some(if down >= 0.0 { 1.0 } else { -1.0 })
    }

    /// Lodge a piece of `drift` as `lodged` has it somewhere along the
    /// stretch the eye looks over, mostly `ahead` of it, at the first of a
    /// few places `dice` draws that has room: where it lay, if one did, or
    /// `None` when the stage will not hold it.
    fn lodge_near(
        &mut self,
        stage: &mut Stage,
        land: &Land,
        drift: &Drift,
        (dice, ahead, lodged): (&mut Dice, f64, Lodged),
    ) -> Option<Lay> {
        for _ in 0..DRIFT_TRIES {
            if stage.room() <= KEPT {
                return Some(Lay::Crowded);
            }
            let along = self.brook.station.along + ahead * dice.range(DRIFT_REACH.0, DRIFT_REACH.1);
            if let lain @ Lay::Lain(_) = self.lodge_at(stage, land, drift, (dice, along, lodged))? {
                return Some(lain);
            }
        }
        Some(Lay::Crowded)
    }

    /// Lodge a piece of `drift` `along` the stream on `land` as `lodged` has
    /// it, drawn from `dice`: where it lay, if it found room, or `None` when
    /// the stage will not hold it.
    fn lodge_at(
        &mut self,
        stage: &mut Stage,
        land: &Land,
        drift: &Drift,
        (dice, along, lodged): (&mut Dice, f64, Lodged),
    ) -> Option<Lay> {
        let course = self.brook.course;
        let section = Section::new(&Station::on(&land.rivers, course, along)?, &self.brook.form);
        let (mark, next) = (
            land.rivers.at(course, along)?,
            land.rivers.at(course, along + 1.0)?,
        );
        let (dx, dz) = (next.x - mark.x, next.z - mark.z);
        let length = mathf::hypot(dx, dz).max(1e-6);
        let (down, left) = ((dx / length, dz / length), (-dz / length, dx / length));
        let downstream = mathf::atan2(down.0, down.1);
        let either = if dice.chance(0.5) { 0.0 } else { PI };
        let side = dice.sign();
        // Each lies as the current left it: where across the course, which
        // way it runs, whether that is where its middle or its foot lies, and
        // whether it is a trunk or a branch.
        let (across, heading, footed, trunk) = match lodged {
            Lodged::Stranded => {
                let bar = if section.thalweg >= 0.0 { -1.0 } else { 1.0 };
                (
                    section.edge(bar) + bar * dice.range(-0.25, 0.35),
                    downstream + either + dice.range(-0.6, 0.6),
                    false,
                    false,
                )
            }
            Lodged::Jammed => (
                dice.range(-0.55, 0.55) * section.half,
                downstream + FRAC_PI_2 + dice.range(-0.5, 0.5),
                false,
                false,
            ),
            Lodged::Sunk => {
                let (near, far) = (section.edge(-1.0), section.edge(1.0));
                (
                    near + (far - near) * dice.range(0.2, 0.8),
                    downstream + either + dice.range(-0.7, 0.7),
                    false,
                    dice.chance(SUNK_TRUNKS),
                )
            }
            Lodged::Leaning => {
                // Its top swung down the current as it fell in.
                let swung = dice.range(0.35, 1.0);
                let into = (
                    -side * left.0 * mathf::cos(swung) + down.0 * mathf::sin(swung),
                    -side * left.1 * mathf::cos(swung) + down.1 * mathf::sin(swung),
                );
                (
                    side * (section.half + dice.range(0.2, 0.9)),
                    mathf::atan2(into.0, into.1),
                    true,
                    dice.chance(LEANING_TRUNKS),
                )
            }
            Lodged::Spanning => (
                side * (section.half + dice.range(0.3, 1.2)),
                mathf::atan2(-side * left.0, -side * left.1) + dice.range(-0.35, 0.35),
                true,
                true,
            ),
        };
        let piece = drift.piece(dice, trunk)?;
        let scale = dice.range(0.8, 1.15);
        let place = (mark.x + left.0 * across, mark.z + left.1 * across);
        let reach = if footed {
            0.0
        } else {
            0.5 * piece.length * scale
        };
        let start = (
            place.0 - mathf::sin(heading) * reach,
            place.1 - mathf::cos(heading) * reach,
        );
        self.lay_wood(
            stage,
            (land, drift),
            (piece, scale),
            (start, heading),
            dice.seed(),
        )
    }

    /// Pile the branches the current brings against `trunk`'s upstream side,
    /// lying along it, as many as `dice` draws; `None` when the stage will not
    /// hold them.
    fn jam(
        &mut self,
        stage: &mut Stage,
        land: &Land,
        drift: &Drift,
        (dice, trunk): (&mut Dice, Lain),
    ) -> Option<()> {
        let (sin, cos) = (mathf::sin(trunk.heading), mathf::cos(trunk.heading));
        for _ in 0..dice.count(JAM.0, JAM.1) {
            let t = dice.range(0.2, 0.8);
            let on = (
                trunk.start.0 + sin * trunk.length * t,
                trunk.start.1 + cos * trunk.length * t,
            );
            let Some(near) = land.rivers.nearest(on.0, on.1) else {
                continue;
            };
            let back = trunk.radius + dice.range(0.05, 0.3);
            let middle = (on.0 - near.toward.0 * back, on.1 - near.toward.1 * back);
            let piece = drift.piece(dice, false)?;
            let scale = dice.range(0.8, 1.15);
            let heading = trunk.heading + dice.range(-0.35, 0.35);
            let reach = 0.5 * piece.length * scale;
            let start = (
                middle.0 - mathf::sin(heading) * reach,
                middle.1 - mathf::cos(heading) * reach,
            );
            self.lay_wood(
                stage,
                (land, drift),
                (piece, scale),
                (start, heading),
                dice.seed(),
            )?;
        }
        Some(())
    }

    /// Lay `piece` of `drift`, `scale` times its size, on `land` from its
    /// foot at `start` along `heading`, resting on the ground or the bed
    /// beneath either end and clear of whatever already stands there, keyed
    /// `key`; and keep what of it lies in the brook's water for the flow.
    /// What came of it, or `None` when the stage will not hold it.
    fn lay_wood(
        &mut self,
        stage: &mut Stage,
        (land, drift): (&Land, &Drift),
        (piece, scale): (Piece, f64),
        (start, heading): ((f64, f64), f64),
        key: u32,
    ) -> Option<Lay> {
        let course = self.brook.course;
        let (long, thick) = (piece.length * scale, piece.radius * scale);
        let (sin, cos) = (mathf::sin(heading), mathf::cos(heading));
        let room = (1.2 * thick).max(0.15);
        let discs =
            u32::try_from(mathf::round_i32(mathf::ceil(long / (1.5 * room))).max(2)).ok()?;
        let disc = |index: u32| {
            let t = f64::from(index) / f64::from(discs - 1);
            ((start.0 + sin * long * t, start.1 + cos * long * t), t)
        };
        if !(0..discs).all(|index| stage.clear(disc(index).0, room)) {
            return Some(Lay::Crowded);
        }
        let tip = disc(discs - 1).0;
        let (foot, top) = (
            land.height(&stage.fields, start.0, start.1),
            land.height(&stage.fields, tip.0, tip.1),
        );
        let pitch = mathf::atan2(top - foot, long);
        let pose = Pose::new(
            Vec3::new(start.0, foot, start.1),
            Frame::turned(heading, -pitch),
        );
        drift.lay(stage, (piece.prototype, scale), (pose, key))?;
        for index in 0..discs {
            let ((x, z), t) = disc(index);
            stage.claim((x, z), room)?;
            // Its axis rests as far over the line between its ends as it is
            // thick there, sunk a little into what it lies on.
            let radius = thick * (1.0 - LOG_TAPER * t);
            let axis = foot + (top - foot) * t + (1.0 - SUNK) * radius;
            let Some(near) = land
                .rivers
                .nearest(x, z)
                .filter(|near| near.course == course)
            else {
                continue;
            };
            let section = Section::new(&Station::of(&near), &self.brook.form);
            let across = near.side * near.distance;
            if near.distance >= section.half || axis - radius >= section.water {
                continue;
            }
            self.stones.try_reserve(1).ok()?;
            self.stones.push(Stone {
                at: (near.along, across),
                reach: (radius, radius),
                top: (axis + radius - section.bed(across)).max(0.01),
            });
        }
        Some(Lay::Lain(Lain {
            start,
            heading,
            length: long,
            radius: thick,
        }))
    }

    /// Every stone set out: let what laying them took go, the drift to lodge.
    fn placed(&mut self) {
        self.found = Runs::default();
        self.ranking = Ranking::default();
        self.laid = Vec::new();
        self.pass = Pass::Lodging;
    }

    /// Set `stone` out on `land`: on its flattest side, its longest across the
    /// stream and dipping upstream, bedded into the gravel, wet where the
    /// water covers it; and, in the brook's own stream, kept for the flow.
    fn set_out(&mut self, stage: &mut Stage, land: &Land, stone: Laid) -> Option<()> {
        let fields = &stage.fields;
        let (x, z) = stone.at;
        let ground = land.height(fields, x, z);
        let near = land.rivers.nearest(x, z)?;
        let (prototype, habit) = *self.kinds.get(stone.wear)?.get(stone.shape)?;
        let scale = 0.5 * stone.size;
        let half = habit.squash * scale;
        let base = Vec3::new(x, ground, z);
        let downstream = mathf::atan2(near.toward.0, near.toward.1);
        let (pose, buried, turn) = if stone.lying == Lying::Bedded {
            // The frame's z runs down the stream, give or take, so the
            // stone's length, its x, lies across it; tilted so its upstream
            // end dips.
            let turn = downstream + 0.5 * (unit(mix32(stone.key ^ 6)) - 0.5);
            let dip = 0.14 + 0.3 * unit(mix32(stone.key ^ 7));
            let buried = 0.2 + 0.2 * unit(mix32(stone.key ^ 8));
            let frame = Frame::turned(turn, -dip);
            (
                Pose::new(base + Vec3::UP * bedded(half, buried), frame),
                buried,
                turn,
            )
        } else {
            // A fallen boulder settles into the ground part way to its slope;
            // an outcrop stands square to the face it juts from, deep in it.
            let (lean, buried) = match stone.lying {
                Lying::Outcropping => (1.0, 0.55 + 0.15 * unit(mix32(stone.key ^ 8))),
                _ => (0.5, 0.3 + 0.15 * unit(mix32(stone.key ^ 8))),
            };
            let upright = Vec3::UP.lerp(land.normal(fields, x, z), lean).normalized();
            let turn = TAU * unit(mix32(stone.key ^ 6));
            let frame = Frame::turned(turn, 0.0).aligning(Vec3::UP, upright);
            (
                Pose::new(base + upright * bedded(half, buried), frame),
                buried,
                turn,
            )
        };
        let top = ground + 2.0 * half * (1.0 - buried);
        let section = Section::new(&Station::of(&near), &self.brook.form);
        let material = if section.water > ground && section.water > top {
            self.wet
        } else if stone.lying == Lying::Bedded {
            self.dry
        } else {
            self.mossy
        };
        stage.add(
            Shape::Instance {
                prototype,
                pose,
                scale,
                key: mix32(stone.key ^ 9),
            },
            material,
            pose,
            false,
        )?;
        if stone.lying != Lying::Bedded {
            // Nothing laid later lies through a boulder or an outcrop.
            stage.claim((x, z), scale)?;
        }
        if near.course == self.brook.course {
            // Against the bed the flow is solved over: the channel's own,
            // the stone reaching along and across it as far as its length
            // and breadth do, turned as it lies.
            let across = near.side * near.distance;
            let bed = section.bed(across);
            let (sin, cos) = (mathf::sin(turn - downstream), mathf::cos(turn - downstream));
            let (long, short) = (scale, habit.elongation * scale);
            self.stones.try_reserve(1).ok()?;
            self.stones.push(Stone {
                at: (near.along, across),
                reach: (
                    mathf::hypot(short * cos, long * sin),
                    mathf::hypot(short * sin, long * cos),
                ),
                top: top - bed,
            });
        }
        Some(())
    }
}

/// Shape the next unit of the rows of `land`'s finer water grid from `row`
/// across `runner` by `flow`, the stream down `course`'s answer to its bed:
/// each place over that stream raised or lowered as the flow has it and
/// carrying the foam on it there. The row to go on from, and whether every
/// row is shaped; `None` when the land has no such grid.
pub(super) fn surface(
    stage: &mut Stage,
    land: &Land,
    (flow, course): (&Flow, usize),
    row: usize,
    runner: &dyn JobRunner,
) -> Option<(usize, bool)> {
    let finer = land.near_water?;
    let field = stage.fields.get_mut(finer.field as usize)?;
    let side = field.side();
    let end = (row + ROWS * runner.width().max(1)).min(side);
    let ((origin_x, origin_z), step) = field.placing();
    field.each_row(row..end, runner, &|(at, heights, kept)| {
        let z = origin_z + step * real(*at);
        for (column, height) in heights.iter_mut().enumerate() {
            if !height.is_finite() {
                continue;
            }
            let x = origin_x + step * real(column);
            let Some(near) = land
                .rivers
                .nearest(x, z)
                .filter(|near| near.course == course)
            else {
                continue;
            };
            // Held to nothing at the grid's seam, where it meets the far
            // water as that stands.
            let own = seam(finer, (x, z));
            let (rise, foam) = flow.at(near.along, near.side * near.distance);
            *height = single(f64::from(*height) + own * rise);
            if let Some(slot) = kept
                .get_mut(column)
                .and_then(|kept| kept.get_mut(CARRIED_FOAM))
            {
                *slot = byte(own * foam);
            }
        }
    });
    Some((end, end >= side))
}

#[cfg(test)]
#[path = "stones_tests.rs"]
mod tests;

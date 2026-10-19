//! Everything a land is made from, chosen before any of it is built, and the
//! lattices it is built on.

use alloc::vec::Vec;
use tairix_countryside::boundary::Style;
use tairix_countryside::site::Villages;
use tairix_countryside::usage::Mix;

use tairix_util::mathf;

use crate::channel::Form;
use crate::relief::Terrain;
use crate::snow::Snowpack;
use crate::{Error, Season};

/// The most levels a land is refined through: from a coarse step of tens of
/// metres to a few centimetres underfoot takes three.
pub const LEVELS: usize = 3;

/// Everything a land is made from.
#[derive(Clone, Debug)]
pub struct Plan {
    /// The relief water wears.
    pub relief: Terrain,
    /// Half the breadth of the land built, about the relief's centre.
    pub reach: f64,
    /// Cells along each side of the coarse lattice the land is worn on.
    pub cells: usize,
    /// The levels refining it, level 0 first, `None` past the last.
    pub levels: [Option<Level>; LEVELS],
    /// The level below which the sea stands, if it does.
    pub sea: Option<f64>,
    /// How water wears the coarse land.
    pub wear: Wear,
    /// How its rivers run, if it has any.
    pub rivers: Option<Rivers>,
    /// A road through the land, which it routes itself.
    pub road: Option<Roadway>,
    /// The countryside laid out over it, about that road if it has one.
    pub farming: Option<Farming>,
    /// How rough the land is below the coarse lattice's step, in metres.
    pub roughness: f64,
    /// How much of that roughness is sharp-crested ridges, as bare rock and
    /// gullied ground break, rather than the rounded hummocks of ground
    /// under a mantle of soil, sand or snow.
    pub ridges: f64,
    /// The height snow lies from, if any does.
    pub snow_line: Option<f64>,
    /// The snow lying over the whole land as the wind laid it, if it does.
    pub snowpack: Option<Snowpack>,
    /// A hollow water keeps, sediment never filling it: its middle, and how
    /// far about it the infill spares.
    pub pond: Option<((f64, f64), f64)>,
    /// How much of the ground grows green where water and slope allow: all
    /// of it in a temperate land, a little in a desert, where only the
    /// wetter ground by its streams grows more.
    pub growth: f64,
    /// The seed every draw of the land is keyed from.
    pub seed: u32,
}

/// A level refining the one before it, or level 0 the coarse land.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Level {
    /// How many of its steps span its parent's: at least one.
    pub ratio: u32,
    /// Droplets run over it, to each vertex they may start on.
    pub droplets: f64,
}

impl Level {
    /// Whether it runs droplets at all: a finite number of them, none below
    /// nought.
    #[must_use]
    pub fn fits(&self) -> bool {
        self.droplets.is_finite() && self.droplets >= 0.0
    }
}

/// How water wears the coarse land.
#[derive(Copy, Clone, Debug)]
pub struct Wear {
    /// Passes of drainage, incision and creep.
    pub passes: u32,
    /// Incision per pass, as erodibility times a time step.
    pub incision: f64,
    /// The share of the gap to its neighbours' mean a sample creeps each pass.
    pub creep: f64,
    /// The steepest slope that stands, as rise over run; steeper slumps.
    pub repose: f64,
    /// The share of a basin's depth below the level it would spill at that
    /// sediment fills each pass, so pits silt up into flats and only the
    /// deepest hold lakes.
    pub infill: f64,
    /// Hard beds every so many metres up, which wear so many times as slowly
    /// and so stand as cliffs above benches: a canyon's.
    pub strata: Option<(f64, f64)>,
}

impl Wear {
    /// Whether it wears the land at all: an incision finite and none below
    /// nought, a creep and an infill each within `0.0..=1.0`, a repose that
    /// is a positive finite slope, and strata a positive finite spacing and
    /// hardness where it has them.
    #[must_use]
    pub fn fits(&self) -> bool {
        let share = 0.0..=1.0;
        let positive = |value: f64| value.is_finite() && value > 0.0;
        self.incision.is_finite()
            && self.incision >= 0.0
            && share.contains(&self.creep)
            && positive(self.repose)
            && share.contains(&self.infill)
            && self
                .strata
                .is_none_or(|(spacing, hardness)| positive(spacing) && positive(hardness))
    }
}

/// How a land's rivers run.
#[derive(Copy, Clone, Debug)]
pub struct Rivers {
    /// The ground a stream must drain, in square metres, to run as a river.
    pub catchment: f64,
    /// A river's breadth where it begins, which grows with the square root
    /// of what it drains past that.
    pub width: f64,
    /// How far it wanders from side to side across level ground, in widths.
    pub meander: f64,
    /// The share of its channel's depth its water fills: one in spate, less
    /// in a dry season, the margins of its bed left bare.
    pub flowing: f64,
    /// The share of its reaches whose fall bedded rock gathers at a ledge.
    pub ledges: f64,
    /// The share of its banks rock outcrops in.
    pub outcrops: f64,
}

impl Rivers {
    /// Whether they run at all: a catchment and a breadth that are positive
    /// finite numbers, a meander finite and none below nought, and their
    /// flow, ledges and outcrops each within `0.0..=1.0`.
    #[must_use]
    pub fn fits(&self) -> bool {
        let share = 0.0..=1.0;
        let positive = |value: f64| value.is_finite() && value > 0.0;
        positive(self.catchment)
            && positive(self.width)
            && self.meander.is_finite()
            && self.meander >= 0.0
            && share.contains(&self.flowing)
            && share.contains(&self.ledges)
            && share.contains(&self.outcrops)
    }

    /// What shapes the channels of a land built under `seed`.
    #[must_use]
    pub fn form(&self, seed: u32) -> Form {
        Form {
            flowing: self.flowing,
            ledges: self.ledges,
            outcrops: self.outcrops,
            seed: seed ^ CHANNELS,
        }
    }
}

/// The key a land's channels are drawn under, against its own.
const CHANNELS: u32 = 0xc4a7;

/// What a way is made of.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Surface {
    /// Tarmac, lined.
    Tarmac,
    /// Loose stone.
    Gravel,
    /// A track of two ruts and the grass between them.
    Track,
}

/// Every surface a way is laid with, the least first: the order a place's
/// ways are carved and painted in, so the greatest has the last say.
pub const SURFACES: [Surface; 3] = [Surface::Track, Surface::Gravel, Surface::Tarmac];

/// A road across a land.
#[derive(Copy, Clone, Debug)]
pub struct Roadway {
    /// What it is made of.
    pub surface: Surface,
    /// How broad it is.
    pub width: f64,
    /// Which way across the land it runs, in radians.
    pub heading: f64,
}

impl Roadway {
    /// Whether it is a road at all: a positive finite breadth on a finite
    /// heading.
    #[must_use]
    pub fn fits(&self) -> bool {
        self.width.is_finite() && self.width > 0.0 && self.heading.is_finite()
    }
}

/// The countryside laid out over a land.
#[derive(Copy, Clone, Debug)]
pub struct Farming {
    /// How far apart its holdings lie.
    pub spacing: f64,
    /// Its villages.
    pub villages: Villages,
    /// Its custom in boundaries.
    pub style: Style,
    /// What its fields are used for.
    pub mix: Mix,
    /// How wooded its country is on the whole.
    pub wooded: f64,
    /// How much stone lies at hand in it wherever it is.
    pub stone: f64,
    /// How far about the land's middle it is laid out.
    pub reach: f64,
    /// The share of its lanes tarmacked, and of the rest the share
    /// gravelled; the others are green lanes, a track's two ruts.
    pub lanes: (f64, f64),
    /// The season its fields stand in.
    pub season: Season,
}

impl Farming {
    /// Whether it farms the land at all: a spacing and a reach that are
    /// positive finite lengths, its woods, stone and lanes each within
    /// `0.0..=1.0`, and its villages, custom and mix such as a countryside
    /// lays out.
    #[must_use]
    pub fn fits(&self) -> bool {
        let share = 0.0..=1.0;
        let positive = |value: f64| value.is_finite() && value > 0.0;
        positive(self.spacing)
            && positive(self.reach)
            && share.contains(&self.wooded)
            && share.contains(&self.stone)
            && share.contains(&self.lanes.0)
            && share.contains(&self.lanes.1)
            && self.villages.fits()
            && self.style.fits()
            && self.mix.fits()
    }
}

/// The coarse lattice a land is worn on: its step, and the lattice place of
/// its first vertex, which lies at whole steps from the land's origin.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Lattice {
    /// How far apart its vertices lie.
    pub step: f64,
    /// Its first vertex, in steps from the land's origin.
    pub origin: (i64, i64),
    /// Its cells a side.
    pub cells: u32,
}

impl Plan {
    /// Whether every number it holds is one a land is built from, as its
    /// relief, wear, levels, rivers, road and farming each say of their own,
    /// and its pond a finite middle and spared reach where it has one.
    #[must_use]
    pub fn fits(&self) -> bool {
        self.relief.fits()
            && self.wear.fits()
            && self.levels.iter().flatten().all(Level::fits)
            && self.rivers.as_ref().is_none_or(Rivers::fits)
            && self.road.as_ref().is_none_or(Roadway::fits)
            && self.farming.as_ref().is_none_or(Farming::fits)
            && self.pond.is_none_or(|((x, z), spared)| {
                x.is_finite() && z.is_finite() && spared.is_finite() && spared >= 0.0
            })
    }

    /// The coarse lattice: `cells` a side over the land's breadth, its
    /// square snapped to whole steps of a lattice fixed to the land; `Shape`
    /// for a plan of no cells, a breadth that is not a positive finite
    /// number, or more cells than a lattice counts.
    pub fn lattice(&self) -> Result<Lattice, Error> {
        let cells = u32::try_from(self.cells).map_err(|_| Error::Shape)?;
        if cells == 0 || !self.reach.is_finite() || self.reach <= 0.0 {
            return Err(Error::Shape);
        }
        let step = 2.0 * self.reach / f64::from(cells);
        let first = |centre: f64| {
            let place = mathf::round((centre - self.reach) / step);
            i64::from(mathf::round_i32(place))
        };
        let (cx, cz) = self.relief.centre;
        Ok(Lattice {
            step,
            origin: (first(cx), first(cz)),
            cells,
        })
    }

    /// The land's lattices: its coarse lattice and the levels refining it.
    pub fn lattices(&self) -> Result<Lattices, Error> {
        Lattices::new(self.lattice()?, self.levels)
    }
}

/// What every vertex of a land's levels is read through but its courses and
/// its grain: the longest wave its levels add below the coarse step, how much
/// grows, the snow and the sea, and its seed.
#[derive(Copy, Clone, Debug)]
pub struct Character {
    /// The longest wave of relief the levels add below the coarse step, in
    /// metres: level 0 adds it and the octaves below, each level after carrying
    /// on down from where the one before left off.
    pub longest: f64,
    /// How much of the ground grows green where water and slope allow.
    pub growth: f64,
    /// The height snow lies from, if any does.
    pub snow_line: Option<f64>,
    /// The level below which the sea stands, if it does.
    pub sea: Option<f64>,
    /// The snow lying over the whole land as the wind laid it, if it does.
    pub snowpack: Option<Snowpack>,
    /// The seed every draw of the land is keyed from.
    pub seed: u32,
}

impl Character {
    /// Whether it describes a land at all: a longest wave that is a positive
    /// finite length, growth within `0.0..=1.0`, a snow line and sea that are
    /// finite levels where they are given, and snow that fell a finite depth
    /// on a finite heading where it lies.
    #[must_use]
    pub fn fits(&self) -> bool {
        let level = |at: Option<f64>| at.is_none_or(f64::is_finite);
        self.longest.is_finite()
            && self.longest > 0.0
            && (0.0..=1.0).contains(&self.growth)
            && level(self.snow_line)
            && level(self.sea)
            && self.snowpack.is_none_or(|pack| {
                pack.heading.is_finite() && pack.fallen.is_finite() && pack.fallen >= 0.0
            })
    }
}

/// The grain of a land's relief below its coarse step at a place: how rough
/// it is, how much of that roughness runs in sharp-crested ridges, billowed
/// dunes or creased gullies rather than rounded hummocks, which of its waves
/// it leans to, and the knobs it is studded with at the scale of a stride.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Grain {
    /// How rough, in metres.
    pub roughness: f64,
    /// The share of the roughness in ridges, as bare rock breaks.
    pub ridges: f64,
    /// The share in dunes, as wind-blown sand lies.
    pub dunes: f64,
    /// The share in gullies, as soft rock is rilled where little grows to
    /// hold it; with the ridges' and the dunes', at most one.
    pub gullies: f64,
    /// How the relief leans across its waves, `-1.0..=1.0`: toward its
    /// longest, rolling in smooth swells as grassland does, toward its
    /// shortest, breaking as frost-split rock and rilled clay do, or neither,
    /// as natural ground is at nought.
    pub lean: f64,
    /// How high, in metres, the knobs stand that frost heaves up in tundra,
    /// tussocks raise in a bog and rubble lies in, waves of a few metres a
    /// level fine enough to hold them adds.
    pub knobs: f64,
}

impl Grain {
    /// Rounded hummocks `roughness` metres rough, leaning to none of its waves
    /// and studded with nothing.
    #[must_use]
    pub const fn hummocks(roughness: f64) -> Self {
        Self {
            roughness,
            ridges: 0.0,
            dunes: 0.0,
            gullies: 0.0,
            lean: 0.0,
            knobs: 0.0,
        }
    }

    /// Whether it is a grain at all: finite, no rougher than nought below, its
    /// shares each within `0.0..=1.0` and together at most one, its lean
    /// within `-1.0..=1.0`, and its knobs a finite height no lower than
    /// nought.
    #[must_use]
    pub fn fits(&self) -> bool {
        let share = 0.0..=1.0;
        self.roughness.is_finite()
            && self.roughness >= 0.0
            && share.contains(&self.ridges)
            && share.contains(&self.dunes)
            && share.contains(&self.gullies)
            && (-1.0..=1.0).contains(&self.lean)
            && self.ridges + self.dunes + self.gullies <= 1.0
            && self.knobs.is_finite()
            && self.knobs >= 0.0
    }
}

/// A land's grain: one grain over the whole land, or one to each coarse
/// sample, row by row, read between them.
#[derive(Clone, Debug)]
pub enum Grains {
    /// The same grain everywhere.
    Even(Grain),
    /// A grain to each coarse sample.
    Sampled(Vec<Grain>),
}

/// A land's lattices: its coarse lattice, and the levels refining it, each on
/// a lattice of its own whose step is its parent's over a whole ratio.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Lattices {
    pub(crate) coarse: Lattice,
    pub(crate) levels: [Option<Level>; LEVELS],
}

impl Lattices {
    /// The coarse lattice `coarse` and `levels` refining it, level 0 first and
    /// `None` past the last: `Shape` for a lattice of no cells, a step that is
    /// not a positive finite number, a ratio of nought, or a level after a
    /// gap.
    pub fn new(coarse: Lattice, levels: [Option<Level>; LEVELS]) -> Result<Self, Error> {
        if coarse.cells == 0 || !coarse.step.is_finite() || coarse.step <= 0.0 {
            return Err(Error::Shape);
        }
        let depth = levels.iter().take_while(|level| level.is_some()).count();
        if levels.iter().skip(depth).any(Option::is_some) {
            return Err(Error::Shape);
        }
        let lattices = Self { coarse, levels };
        for level in 0..depth {
            lattices.domain(level)?;
        }
        Ok(lattices)
    }

    /// The coarse lattice.
    #[must_use]
    pub const fn coarse(&self) -> Lattice {
        self.coarse
    }

    /// How many levels refine the coarse lattice.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.levels
            .iter()
            .take_while(|level| level.is_some())
            .count()
    }

    /// How many of its own steps level `level`'s lattice puts in one of the
    /// coarse lattice's; `Shape` for a level the land is not refined to.
    pub fn refinement(&self, level: usize) -> Result<u32, Error> {
        let mut product = 1_u32;
        for slot in self.levels.get(..=level).ok_or(Error::Shape)? {
            let level = slot.filter(|level| level.ratio > 0).ok_or(Error::Shape)?;
            product = product.checked_mul(level.ratio).ok_or(Error::Shape)?;
        }
        Ok(product)
    }
}

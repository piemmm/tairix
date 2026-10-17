//! The levels refining a land, and pieces of them.
//!
//! Each level lies on a lattice fixed to the land, its step its parent's over
//! a whole ratio, so every level's vertices stand at fixed places and every
//! ratio-th of them on its parent's. A piece is a square of a level's
//! lattice, and a pure function of the land and the square: its ground before
//! the droplets is its parent read through a Catmull–Rom patch, the level's
//! band of detail keyed by place, the beds the land's courses cut and the snow
//! as the level lies it; its droplets run on the level's lattice over the
//! land's square, a halo held about the piece so it wears as the land worn
//! whole does (`tairix_terrain::droplet`); then its channels and ways are put
//! back to their beds, its clearing and cuttings held to their levels, and on
//! level 0 its vertices take the wet and the silt the droplets left.

use alloc::vec::Vec;

use tairix_countryside::Point;
use tairix_mix::numbered;
use tairix_noise::{billow2, fbm2, noise2, ridged2, smoothstep};
use tairix_parallel::JobRunner;
use tairix_terrain::droplet::{Droplets, Placed, Source, Store, Tiles, Unkept, Wearing, Work};
use tairix_terrain::TerrainError;
use tairix_util::fallible;
use tairix_util::mathf::{self, byte, real, share, single};

use crate::attributes::{self, Attributes, Lie, GREEN, QUANTITIES, SEDIMENT, SNOW, WET};
use crate::channel::{Banked, Station};
use crate::coarse::Coarse;
use crate::grown::{self, Grown};
use crate::plan::{Grain, Lattices, Level, SURFACES};
use crate::relief::Terrain;
use crate::river::{delta_bed, river_bed, say, BRIDGE_REACH, SCOUR_WANDER};
use crate::snow::{self, Snowpack};
use crate::ways::{cutting_bed, ditch_bed, path_worn, road_bed};
use crate::{run_cells, unit_end, Error};

/// Vertices of a piece's held square one core fills or settles in a unit of
/// work, whole rows or not: a row's breadth follows the piece, its vertices'
/// cost the land they lie in.
const UNIT_VERTICES: usize = 1024;

/// What one item of each kind of a piece's work costs, in nanoseconds on a
/// desktop core preparing across eight threads: a vertex filled, a droplet
/// run, a vertex settled after the droplets. Measured; only their proportions
/// matter, to weigh a piece's stages against each other and against the
/// coarse build.
const FILL_NS: f64 = 60.0;
const DROPLET_NS: f64 = 1_040.0;
const SETTLE_NS: f64 = 50.0;

/// The longest wave of the detail level 0 adds to a coarse land it wore
/// itself, in the coarse lattice's steps.
pub(crate) const COARSE_DETAIL: f64 = 3.0;

/// How tall the winter grass stands that snow buries, in metres: thinner
/// snow than this leaves its tops showing.
const GRASS_BURIED: f64 = 0.14;

/// How far above the sea the ground begins to shelve away beneath it.
const SHELF: f64 = 0.3;

/// How far beneath that the shore's face shelves away more steeply than the
/// land: about as far down as waves work a beach's face.
const SHORE_FACE: f64 = 10.0;

/// How far beyond what a piece's ground reads of its parent a Catmull–Rom
/// patch reaches: one vertex before a cell and two after it.
const PATCH_BEFORE: i64 = 1;
const PATCH_AFTER: i64 = 2;

/// A level as a parent holds it for a piece of the level after it to be
/// refined from: a square of its lattice, its heights and what each vertex
/// carries, row by row.
#[derive(Copy, Clone, Debug)]
pub struct Parent<'a> {
    /// The square of the parent level's lattice held.
    pub square: Placed,
    /// Its heights, row by row.
    pub heights: &'a [f32],
    /// What each of its vertices carries, row by row; or nothing, from a
    /// consumer that keeps only its heights, its vertices then read as plain
    /// ground ([`attributes::PLAIN`]) — on a land where no snow lies, since a
    /// parent's snow is taken out of the ground beneath its child's.
    pub attributes: &'a [Attributes],
}

/// What a piece's refinement keeps of its vertices.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Keeps {
    /// Their heights and what each carries.
    Everything,
    /// Their heights alone, the same to the bit, for a consumer reading
    /// nothing else: it carries no attributes, so a level after it reads it
    /// as plain ground, which it can only where no snow lies.
    Heights,
}

/// A square of a level, refined.
#[derive(Debug)]
pub struct Piece {
    /// Which level.
    pub level: usize,
    /// The square of the level's lattice it covers.
    pub square: Placed,
    /// Its heights, row by row.
    pub heights: Vec<f32>,
    /// What each of its vertices carries, row by row; nothing where the
    /// refinement kept only its heights.
    pub attributes: Vec<Attributes>,
}

impl Piece {
    /// The piece as a parent of the level after it.
    #[must_use]
    pub fn parent(&self) -> Parent<'_> {
        Parent {
            square: self.square,
            heights: &self.heights,
            attributes: &self.attributes,
        }
    }
}

impl Lattices {
    /// How far apart level `level`'s vertices lie; `Shape` for a level the
    /// land is not refined to.
    pub fn level_step(&self, level: usize) -> Result<f64, Error> {
        Ok(self.coarse().step / f64::from(self.refinement(level)?))
    }

    /// The square of level `level`'s lattice the land covers.
    pub fn domain(&self, level: usize) -> Result<Placed, Error> {
        let lattice = self.coarse();
        let ratio = self.refinement(level)?;
        let wide = i64::from(ratio);
        let origin = (
            lattice.origin.0.checked_mul(wide).ok_or(Error::Shape)?,
            lattice.origin.1.checked_mul(wide).ok_or(Error::Shape)?,
        );
        let side = lattice
            .cells
            .checked_mul(ratio)
            .and_then(|cells| cells.checked_add(1))
            .ok_or(Error::Shape)?;
        let placed = Placed::square(origin, side);
        // Every place a piece reads is a lattice index an `i32` holds, so it
        // converts to a world place exactly.
        let (x1, z1) = placed.end()?;
        for corner in [origin.0, origin.1, x1, z1] {
            i32::try_from(corner).map_err(|_| Error::Shape)?;
        }
        Ok(placed)
    }

    /// Level `level`'s droplets over its land as a run of tiles, drawn under
    /// `key`.
    pub(crate) fn tiles(&self, level: usize, key: u64) -> Result<Tiles, Error> {
        let step = self.level_step(level)?;
        let rate = self.level(level)?.droplets;
        Ok(Tiles::new(
            droplets(step),
            (rate, key),
            self.domain(level)?,
        )?)
    }

    /// How far beyond a piece of level `level` the ground it is worn from can
    /// lie, at most, in the level's vertices.
    pub fn halo(&self, level: usize) -> Result<u32, Error> {
        Ok(self.tiles(level, 0)?.halo()?)
    }

    /// The square of level `level`'s lattice a piece `square` is worn from
    /// where nothing of the level is kept: the blocks its ground is worn in
    /// and its tiles' windows, within the land.
    pub fn held(&self, level: usize, square: Placed) -> Result<Placed, Error> {
        let wearing = Wearing::new(&self.tiles(level, 0)?, (square, level == 0), &mut Unkept)?;
        Ok(wearing.fills().unwrap_or(square))
    }

    /// The square of level `level - 1`'s lattice a piece `square` of level
    /// `level` reads its parent over: everything a Catmull–Rom patch about
    /// any vertex of its held square reaches, within the land. `Shape` for
    /// level 0, which reads the coarse land itself.
    pub fn needs(&self, level: usize, square: Placed) -> Result<Placed, Error> {
        let parent = level.checked_sub(1).ok_or(Error::Shape)?;
        let ratio = i64::from(self.ratio(level)?);
        let held = self.held(level, square)?;
        let (end_x, end_z) = held.end()?;
        let low = |at: i64| at.div_euclid(ratio) - PATCH_BEFORE;
        let high = |at: i64| (at - 1).div_euclid(ratio) + PATCH_AFTER + 1;
        let (x0, z0) = (low(held.origin.0), low(held.origin.1));
        let size = |from: i64, to: i64| u32::try_from(to - from).map_err(|_| Error::Shape);
        Placed {
            origin: (x0, z0),
            size: (size(x0, high(end_x))?, size(z0, high(end_z))?),
        }
        .within(self.domain(parent)?)
        .ok_or(Error::Shape)
    }

    /// What refining the square `square` of level `level` costs, about as
    /// [`Refining::cost`] will count it: so a caller can weigh the pieces it
    /// will refine before it refines them.
    pub fn cost(&self, level: usize, square: Placed) -> Result<f64, Error> {
        let wearing = Wearing::new(&self.tiles(level, 0)?, (square, level == 0), &mut Unkept)?;
        Ok(costs(wearing.work().0, real(square.area()?)))
    }

    /// Level `level`.
    pub fn level(&self, level: usize) -> Result<Level, Error> {
        self.levels
            .get(level)
            .copied()
            .flatten()
            .filter(|level| level.ratio > 0)
            .ok_or(Error::Shape)
    }

    /// Level `level`'s ratio to its parent, or to the coarse lattice.
    pub fn ratio(&self, level: usize) -> Result<u32, Error> {
        Ok(self.level(level)?.ratio)
    }
}

/// The waves of detail a level adds: its longest, how many octaves down from
/// it, and how strongly a wave that long stands against the land's longest.
#[derive(Copy, Clone, Debug)]
struct Band {
    longest: f64,
    octaves: u32,
    spectrum: f64,
}

impl Coarse {
    /// The detail level `level` adds: each level carries on down the
    /// spectrum where the level before it left off, to as short a wave as it
    /// holds well.
    fn band(&self, level: usize) -> Result<Band, Error> {
        let mut longest = self.character.longest;
        let mut octaves = octaves_between(longest, self.lattices.level_step(0)?);
        for finer in 1..=level {
            longest /= mathf::exp(f64::from(octaves) * core::f64::consts::LN_2);
            octaves = octaves_between(longest, self.lattices.level_step(finer)?);
        }
        Ok(self.spanning(longest, octaves))
    }

    /// The band from waves `longest` long down through `octaves` octaves:
    /// fainter the shorter its waves, as natural relief is.
    fn spanning(&self, longest: f64, octaves: u32) -> Band {
        Band {
            longest,
            octaves,
            spectrum: mathf::exp(0.8 * mathf::ln(longest / self.character.longest)),
        }
    }

    /// The fine relief a coarser level is too coarse to hold, of `grain` and
    /// in `band`: roughest on steep ground.
    fn detail(&self, (x, z): (f64, f64), (slope, grain): (f64, Grain), band: Band) -> f64 {
        let Band {
            longest,
            octaves,
            spectrum,
        } = band;
        if octaves == 0 {
            return 0.0;
        }
        let rough = spectrum * grain.roughness * (0.25 + 0.75 * smoothstep(0.05, 0.6, slope));
        let (u, v, seed) = (x / longest, z / longest, self.character.seed);
        let hummocks = fbm2(u, v, seed ^ 0x4a11, (octaves, 0.5, 2.0));
        let ridges = if grain.ridges > 0.0 {
            ridged2(u, v, seed ^ 0xde7a, octaves) - 0.5
        } else {
            0.0
        };
        let mut form = grain.ridges * ridges + (1.0 - grain.ridges - grain.dunes) * hummocks;
        if grain.dunes > 0.0 {
            // Billowed octaves lie about a quarter on average, so the dunes
            // neither raise nor lower the land.
            form += grain.dunes * (2.0 * billow2(u, v, seed ^ 0xd0e5, octaves) - 0.5);
        }
        rough * form
    }

    /// `height` shelving away beneath the sea, if the land has one, so the
    /// ground a hand's breadth under the water lies well under it rather
    /// than meeting the surface all across a flat; deeper than the shore's
    /// face, the sea floor lies as the land does.
    fn shelve(&self, height: f64) -> f64 {
        match self.character.sea {
            Some(sea) if height < sea + SHELF => {
                height - 3.0 * mathf::fmin(sea + SHELF - height, SHORE_FACE)
            }
            _ => height,
        }
    }

    /// Level 0's ground at `(x, z)`, its vertices `step` apart and holding
    /// the `band` of detail it adds to the worn coarse land: its height, and
    /// what it is like.
    fn coarse_ground(&self, (x, z): (f64, f64), (step, band): (f64, Band)) -> (f64, Lie) {
        let (worn, dx, dz) = self.worn_sloped(x, z);
        let slope = mathf::hypot(dx, dz);
        // The relief holds its clearing already; what this level adds to it
        // fades out there, so the clearing stays as level as the relief left it.
        let keep = self.kept((x, z));
        let grain = self.grain((x, z));
        let natural = worn + keep * self.detail((x, z), (slope, grain), band);
        let (carved, lie, channel) = self.carve((x, z), natural, step);
        // Snow lies down to the clearing's water, unlike the land's own detail.
        let snow = self.character.snowpack.map_or(0.0, |pack| {
            let (shelter, _, _) = self.coarse_spline(&self.shelter, (x, z));
            pack.depth(shelter, (x, z), slope)
        });
        let height = natural + keep * (self.shelve(carved) - natural) + snow;
        let height = self.pinned((x, z), height);
        // Read between the coarse samples, so it never steps from one to the
        // next along the coarse lattice's lines.
        let laid = |index: usize| match (self.height.get(index), self.original.get(index)) {
            (Some(now), Some(was)) => {
                ((now - was) / mathf::fmax(grain.roughness, 1.0)).clamp(-1.0, 1.0)
            }
            _ => 0.0,
        };
        let wet = mathf::fmax(self.drained((x, z)), lie.wet);
        let sediment = mathf::fmax(self.between((x, z), &laid), lie.sediment);
        let lie = Lie {
            wet,
            sediment: sediment + (channel.0 - sediment) * channel.1,
            upright: 1.0 / mathf::sqrt(1.0 + slope * slope),
            snow,
            ..lie
        };
        (height, lie)
    }

    /// A finer level's ground at its vertex `at`, which lies at `(x, z)`,
    /// its parent read through `parent`, its vertices `step` apart and
    /// holding the `band` of detail it adds: its height, and what it is like.
    fn finer_ground(
        &self,
        parent: &Reading<'_>,
        (at, (x, z)): ((i64, i64), (f64, f64)),
        (step, band): (f64, Band),
    ) -> (f64, Lie) {
        let (smooth, dx, dz) = parent.cubic(at);
        let slope = mathf::hypot(dx, dz);
        let keep = self.kept((x, z));
        let quantities = parent.quantities(at);
        let (wet, sediment, snow) = (quantities[WET], quantities[SEDIMENT], quantities[SNOW]);
        // The snow as this level lies it, drifted against what stands about
        // it as finely as it resolves, in place of the coarser level's.
        let inherited = snow::depth_of(snow);
        let lying = self.character.snowpack.map_or(inherited, |pack| {
            let (shelter, _, _) = self.coarse_spline(&self.shelter, (x, z));
            let open = pack.depth(shelter, (x, z), slope);
            (open + self.drifted(&pack, (x, z), open)).clamp(0.0, snow::DEEPEST)
        });
        let windworn = self
            .character
            .snowpack
            .map_or(0.0, |pack| pack.carved((x, z), lying, step));
        let grain = self.grain((x, z));
        let own = smooth - inherited
            + lying
            + keep * self.detail((x, z), (slope, grain), band)
            + windworn;
        let (carved, lie, channel) = self.carve((x, z), own, step);
        let height = self.pinned((x, z), own + keep * (self.shelve(carved) - own));
        let sediment = mathf::fmax(attributes::sediment_of(sediment), lie.sediment);
        let lie = Lie {
            wet: mathf::fmax(wet, lie.wet),
            sediment: sediment + (channel.0 - sediment) * channel.1,
            upright: 1.0 / mathf::sqrt(1.0 + slope * slope),
            snow: lying + windworn,
            ..lie
        };
        (height, lie)
    }

    /// The land beyond its square sampled `step` apart, where it was raised
    /// from a relief that runs on past it; `None` for a land brought from
    /// outside, which ends at its square.
    #[must_use]
    pub fn beyond(&self, step: f64) -> Option<Beyond<'_>> {
        let longest = 8.0 * step;
        Some(Beyond {
            coarse: self,
            relief: self.relief.as_ref()?,
            step,
            band: self.spanning(longest, octaves_between(longest, step)),
        })
    }

    /// `natural` ground at `(x, z)` with river beds and the road's bed cut
    /// into it, and what the cutting leaves the place like; `step` the
    /// level's, which the cut is softened across. And what a river's channel
    /// laid down or wore away there, with how much of the say it has over
    /// what was laid, the rest the land's own.
    fn carve(&self, (x, z): (f64, f64), natural: f64, step: f64) -> (f64, Lie, (f64, f64)) {
        let mut height = natural;
        // Silt laid by nothing yet: the least, so the land's own wear stands
        // where no delta lays any over it.
        let mut lie = Lie {
            green: 1.0,
            sediment: -1.0,
            ..Lie::default()
        };
        for delta in &self.deltas {
            let (fan, laid) = delta_bed(delta, (x, z), height);
            height = fan;
            lie.sediment = mathf::fmax(lie.sediment, laid);
            lie.wet = mathf::fmax(lie.wet, 0.8 * laid);
        }
        let mut channel = (0.0, 0.0);
        let river = self.rivers.nearest(x, z);
        if let (Some(near), Some(form)) = (&river, self.form()) {
            let banked = Banked::new(&Station::of(near), &form);
            let wander = noise2(
                x / SCOUR_WANDER,
                z / SCOUR_WANDER,
                self.character.seed ^ 0x5c0f,
            );
            let bedding = river_bed(&banked, near, mathf::fmin(natural, height), (step, wander));
            height = bedding.height;
            lie.wet = mathf::fmax(lie.wet, bedding.wet);
            lie.green *= 1.0 - bedding.scoured;
            channel = (bedding.laid, bedding.say);
        }
        // A road bridges a river rather than filling it, and its bridge
        // carries it; a track or path fords it: the ground beneath is the
        // river's either way.
        let bridging = river.is_some_and(|river| river.distance < 0.5 * river.width + BRIDGE_REACH);
        if !bridging {
            for surface in SURFACES {
                if let Some(near) = self.ways.of(surface).nearest(x, z) {
                    let (bed, worn) = road_bed(&near, height, (surface, step));
                    height = bed;
                    lie.road = mathf::fmax(lie.road, worn.bare);
                    lie.path = mathf::fmax(lie.path, worn.trodden);
                }
            }
            for paths in [&self.paths, &self.ways.trodden] {
                if let Some(near) = paths.nearest(x, z) {
                    let worn = path_worn(&near, step);
                    height -= 0.08 * worn;
                    lie.path = mathf::fmax(lie.path, worn);
                }
            }
            if let Some(near) = self.ways.churned.nearest(x, z) {
                // Hooves tread a gateway to bare mud, pocked where they sink.
                let churned = path_worn(&near, step);
                let pocked = noise2(x * 1.7, z * 1.7, self.character.seed ^ 0x6a7e);
                height -= churned * (0.05 + 0.035 * pocked);
                lie.path = mathf::fmax(lie.path, 0.95 * churned);
                lie.wet = mathf::fmax(lie.wet, 0.8 * churned);
            }
        }
        if let Some(near) = self.ways.ditches.nearest(x, z) {
            let (bed, water) = ditch_bed(&near, height);
            height = mathf::fmin(height, bed);
            lie.wet = mathf::fmax(lie.wet, water);
        }
        if let Some(near) = self.cuttings.nearest(x, z) {
            height = mathf::fmin(height, cutting_bed(&near));
        }
        (lie.grown, lie.rows) = self.grown_at((x, z));
        if Grown::of(lie.grown).bare() {
            lie.green = 0.0;
        }
        (height, lie, channel)
    }

    /// What the field at `(x, z)` grows in the land's season and the way its
    /// rows run, as `grown` codes them: nothing farmed off a field, nor on
    /// the margin a tilled one leaves along its edge.
    fn grown_at(&self, (x, z): (f64, f64)) -> (u8, u8) {
        let (Some(layout), Some(season)) = (&self.layout, self.season) else {
            return (0, 0);
        };
        let at = Point::new(x, z);
        let Some(parcel) = layout.parcel_at(at, self) else {
            return (0, 0);
        };
        let (growth, margin) = grown::sown(parcel, season, self.character.seed);
        if parcel.field.cell.inset(at) < margin {
            return (0, 0);
        }
        (growth.code(), grown::rows_of(&parcel.field))
    }

    /// What a vertex at `height` that `lie` describes carries.
    fn attributes(&self, lie: &Lie, height: f64) -> Attributes {
        let steep = smoothstep(0.62, 0.45, lie.upright);
        let snowed = mathf::fmax(
            self.character
                .snow_line
                .map_or(0.0, |line| smoothstep(line - 40.0, line + 40.0, height)),
            snow::buries(lie.snow, GRASS_BURIED),
        );
        let shore = self
            .character
            .sea
            .map_or(0.0, |sea| smoothstep(sea + 2.5, sea + 0.3, height));
        let growth = self.character.growth;
        let watered = growth + (1.0 - growth) * smoothstep(0.3, 0.8, lie.wet);
        let green = watered
            * lie.green
            * (1.0 - steep)
            * (1.0 - lie.road)
            * (1.0 - 0.8 * lie.path)
            * (1.0 - shore)
            * (1.0 - snowed)
            * (1.0 - smoothstep(0.85, 1.0, lie.wet));
        [
            byte(lie.wet),
            byte(attributes::sediment_share(lie.sediment)),
            byte(lie.road),
            byte(lie.path),
            byte(green),
            snow::kept(lie.snow),
            lie.grown,
            lie.rows,
        ]
    }

    /// How much deeper than `open` the snow `pack` lies at `(x, z)` for the
    /// boundaries it drifts against there: the deepest drift any of them
    /// drops, less the deepest scour any leaves.
    fn drifted(&self, pack: &Snowpack, (x, z): (f64, f64), open: f64) -> f64 {
        let wind = (mathf::sin(pack.heading), mathf::cos(pack.heading));
        let (mut banked, mut scoured) = (0.0_f64, 0.0_f64);
        self.drifts.courses.each_nearest(x, z, |near| {
            let Some(&barrier) = self.drifts.barriers.get(near.course) else {
                return;
            };
            // The wind's share across the barrier toward its left, where a
            // place's side counts positive.
            let (tx, tz) = near.toward;
            let square = wind.1 * tx - wind.0 * tz;
            let downwind =
                near.side * near.distance / mathf::fmax(square.abs(), 0.15).copysign(square);
            let ends = 1.0 - smoothstep(0.0, barrier.height, near.past);
            let change = ends * pack.drifted(barrier, (downwind, square), open);
            banked = mathf::fmax(banked, change);
            scoured = mathf::fmin(scoured, change);
        });
        banked + scoured
    }

    /// `slot`, a vertex at `(x, z)` on a level `step` apart, put back toward
    /// what it stood at, `was`, before the droplets ran, as far as a river's
    /// channel or a way's bed has its say there: floods shape a channel and
    /// its banks' faces, and a way's bed is laid and kept, where droplets
    /// would gully both. A height absent either side is left as it is.
    fn keep_channel(&self, slot: &mut f32, was: f32, (at, step): ((f64, f64), f64)) {
        if slot.is_finite() && was.is_finite() {
            let say = mathf::fmax(self.channel_say(at, step), self.way_say(at, step));
            *slot += single(say) * (was - *slot);
        }
    }

    /// How much of the say a way's bed has over the ground at `(x, z)` on a
    /// level `step` apart: all of it across its breadth, fading across its
    /// verges; a path's, a ditch's and a churned gateway's as far as it is
    /// worn.
    fn way_say(&self, (x, z): (f64, f64), step: f64) -> f64 {
        let surfaced = SURFACES
            .iter()
            .filter_map(|&surface| self.ways.of(surface).nearest(x, z))
            .map(|near| 1.0 - smoothstep(0.5 * near.width, 1.5 * near.width + step, near.distance));
        let trodden = [
            &self.paths,
            &self.ways.trodden,
            &self.ways.churned,
            &self.ways.ditches,
        ]
        .into_iter()
        .filter_map(|paths| paths.nearest(x, z))
        .map(|near| path_worn(&near, step));
        surfaced.chain(trodden).fold(0.0, mathf::fmax)
    }

    /// How much of the say a river's channel has over the ground at
    /// `(x, z)` on a level `step` apart: what it carved there stands against
    /// the droplets run over the land after.
    fn channel_say(&self, (x, z): (f64, f64), step: f64) -> f64 {
        match (self.rivers.nearest(x, z), self.form()) {
            (Some(near), Some(form))
                if near.distance < crate::channel::farthest_say(near.width, step) =>
            {
                say(&Banked::new(&Station::of(&near), &form), &near, step)
            }
            _ => 0.0,
        }
    }

    /// How much of the land's own relief shows at `at`: none within its
    /// clearing, all of it where it has none.
    fn kept(&self, (x, z): (f64, f64)) -> f64 {
        self.clearing.map_or(1.0, |clearing| clearing.keep(x, z))
    }

    /// `height` at `at`, pinned to the clearing's level within it.
    fn pinned(&self, (x, z): (f64, f64), height: f64) -> f64 {
        self.clearing
            .map_or(height, |clearing| clearing.pin(x, z, height))
    }

    /// `slot`, a vertex at `(x, z)`, held to what was made of the land
    /// whatever the droplets did to it: level in the relief's clearing, if it
    /// has one, and down to a cutting's bed, which silt settling in a level
    /// trench would otherwise fill.
    fn hold(&self, slot: &mut f32, (x, z): (f64, f64)) {
        if slot.is_finite() {
            let mut height = self.pinned((x, z), f64::from(*slot));
            if let Some(near) = self.cuttings.nearest(x, z) {
                height = mathf::fmin(height, cutting_bed(&near));
            }
            *slot = single(height);
        }
    }
}

/// The land beyond a coarse land's square, out to the horizon, sampled a
/// step apart.
#[derive(Copy, Clone, Debug)]
pub struct Beyond<'a> {
    coarse: &'a Coarse,
    relief: &'a Terrain,
    step: f64,
    /// The coarsest of the land's detail, the band of its sampling.
    band: Band,
}

impl Beyond<'_> {
    /// The land at `(x, z)`: the relief and the coarsest of the land's
    /// detail, with what grows there as the land's own ground is judged.
    /// Water has not worn the land that far off, and the haze would hide it
    /// if it had.
    #[must_use]
    pub fn unworn(&self, (x, z): (f64, f64)) -> (f64, Attributes) {
        let step = self.step;
        let rise = |dx: f64, dz: f64| self.relief.height(x + dx, z + dz);
        let slope = mathf::hypot(
            rise(step, 0.0) - rise(-step, 0.0),
            rise(0.0, step) - rise(0.0, -step),
        ) / (2.0 * step);
        let grain = self.coarse.grain((x, z));
        let height = rise(0.0, 0.0) + self.coarse.detail((x, z), (slope, grain), self.band);
        let lie = Lie {
            upright: 1.0 / mathf::sqrt(1.0 + slope * slope),
            green: 1.0,
            ..Lie::default()
        };
        (height, self.coarse.attributes(&lie, height))
    }
}

/// What a piece's refinement has still to do.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Stage {
    /// Filling its own vertices from the vertex numbered, row by row.
    Own(usize),
    /// Wearing it with its level's droplets.
    Wear,
    /// Settling its own vertices from the vertex numbered, row by row.
    Settle(usize),
    Done,
}

/// A piece of a level being refined, a bounded unit of work at a time.
#[derive(Debug)]
pub struct Refining {
    level: usize,
    square: Placed,
    /// The parent level's square it reads, after level 0.
    reads: Option<Placed>,
    step: f64,
    band: Band,
    tiles: Tiles,
    wearing: Option<Wearing>,
    /// What its wearing does in all.
    work: Work,
    /// Its own vertices: what each carries where it keeps that, and its
    /// height before the droplets ran and after; and on level 0 alone, where
    /// it wets and greens the ground, the water they carried past each.
    attributes: Vec<Attributes>,
    before: Vec<f32>,
    heights: Vec<f32>,
    flux: Vec<f32>,
    stage: Stage,
}

/// A run of a piece's own vertices being filled: the number of its first,
/// what each carries where it keeps that, and its height before the
/// droplets.
type Filled<'a> = (usize, Option<&'a mut [Attributes]>, &'a mut [f32]);

/// A run of a piece's own vertices being settled: the number of its first,
/// their heights, what each carries where it keeps that, their heights
/// before the droplets, and the water the droplets carried past each.
type Settled<'a> = (
    usize,
    &'a mut [f32],
    Option<&'a mut [Attributes]>,
    &'a [f32],
    &'a [f32],
);

/// `carried`'s runs of `count` vertices, then none: none at all for a piece
/// keeping only its heights.
fn carried_runs(
    carried: &mut [Attributes],
    count: usize,
) -> impl Iterator<Item = Option<&mut [Attributes]>> {
    carried
        .chunks_mut(count.max(1))
        .map(Some)
        .chain(core::iter::repeat_with(|| None))
}

/// The lattice places of the run of `count` vertices from the one numbered
/// `first` of the square `square`, `across` a row, each with its place in the
/// run.
fn run_places(
    square: Placed,
    across: usize,
    run: (usize, usize),
) -> impl Iterator<Item = (usize, (i64, i64))> {
    let lattice =
        |origin: i64, index: usize| origin.saturating_add(i64::try_from(index).unwrap_or(i64::MAX));
    run_cells(across, run)
        .enumerate()
        .map(move |(offset, (column, row))| {
            (
                offset,
                (
                    lattice(square.origin.0, column),
                    lattice(square.origin.1, row),
                ),
            )
        })
}

/// The ground a level's piece is worn from before its droplets run: the
/// coarse land at level 0, after it the parent read through `reading`; and
/// the piece's own square once filled, so no vertex is filled twice.
struct Ground<'a> {
    coarse: &'a Coarse,
    reading: Option<Reading<'a>>,
    step: f64,
    band: Band,
    own: Option<(Placed, &'a [f32])>,
}

impl Ground<'_> {
    /// The ground at the level's vertex `at`: its height, and what it is
    /// like.
    fn at(&self, at: (i64, i64)) -> (f64, Lie) {
        let place = (
            lattice_real(at.0) * self.step,
            lattice_real(at.1) * self.step,
        );
        match &self.reading {
            None => self.coarse.coarse_ground(place, (self.step, self.band)),
            Some(reading) => self
                .coarse
                .finer_ground(reading, (at, place), (self.step, self.band)),
        }
    }
}

impl Source for Ground<'_> {
    fn fill(&self, region: Placed, into: &mut [f32]) -> Result<(), TerrainError> {
        let across = usize::try_from(region.size.0).map_err(|_| TerrainError::Shape)?;
        let index = |at: usize| i64::try_from(at).map_err(|_| TerrainError::Shape);
        for (row, heights) in into.chunks_mut(across.max(1)).enumerate() {
            let z = region.origin.1 + index(row)?;
            for (column, slot) in heights.iter_mut().enumerate() {
                let at = (region.origin.0 + index(column)?, z);
                let own = self
                    .own
                    .and_then(|(square, before)| before.get(square.index(at)?));
                *slot = match own {
                    Some(&height) => height,
                    None => single(self.at(at).0),
                };
            }
        }
        Ok(())
    }
}

/// What a piece's refinement costs, of a wearing doing `work` and of `own`
/// vertices of its own filled and settled.
fn costs(work: Work, own: f64) -> f64 {
    let (filled, droplets) = (real_count(work.filled), real_count(work.droplets));
    (filled + own) * FILL_NS + droplets * DROPLET_NS + own * SETTLE_NS
}

/// A count of work as a real number.
fn real_count(count: u64) -> f64 {
    real(usize::try_from(count).unwrap_or(usize::MAX))
}

impl Refining {
    /// The refinement of the square `square` of level `level` of `coarse`,
    /// keeping what `keeps` names, worn from what `store` keeps of the level
    /// where it keeps it: `Shape` for a level the land is not refined to or a
    /// square off it, `OutOfMemory` when the heap will not hold it.
    pub fn new(
        coarse: &Coarse,
        (level, square): (usize, Placed),
        keeps: Keeps,
        store: &mut dyn Store,
    ) -> Result<Self, Error> {
        let lattices = &coarse.lattices;
        let reads = if level == 0 {
            None
        } else {
            Some(lattices.needs(level, square)?)
        };
        let tiles = lattices.tiles(level, droplet_key(coarse.character.seed, level))?;
        let carries = keeps == Keeps::Everything;
        // Level 0 alone keeps the water the droplets carried, for what it
        // carries.
        let wearing = Wearing::new(&tiles, (square, level == 0 && carries), store)?;
        let own = square.area()?;
        Ok(Self {
            level,
            square,
            reads,
            step: lattices.level_step(level)?,
            band: coarse.band(level)?,
            tiles,
            work: wearing.work().0,
            wearing: Some(wearing),
            attributes: if carries {
                fallible::filled(own, [0; attributes::CHANNELS]).ok_or(Error::OutOfMemory)?
            } else {
                Vec::new()
            },
            before: fallible::filled(own, 0.0).ok_or(Error::OutOfMemory)?,
            heights: Vec::new(),
            flux: Vec::new(),
            stage: Stage::Own(0),
        })
    }

    /// The square of its level it refines.
    #[must_use]
    pub const fn square(&self) -> Placed {
        self.square
    }

    /// The square of the parent level it reads, after level 0.
    #[must_use]
    pub const fn reads(&self) -> Option<Placed> {
        self.reads
    }

    /// Do the next unit of the refinement across `runner`, `parent` the level
    /// before it, holding at least the square `reads` names, or `None` for
    /// level 0, and `store` keeping what it may of the level: whether the
    /// piece is refined. `Shape` for a parent that does not hold what the
    /// piece reads; a refused unit leaves the refinement where it was.
    pub fn step(
        &mut self,
        coarse: &Coarse,
        parent: Option<Parent<'_>>,
        store: &mut dyn Store,
        runner: &dyn JobRunner,
    ) -> Result<bool, Error> {
        self.stage = match self.stage {
            Stage::Own(row) => self.own(coarse, parent, row, runner)?,
            Stage::Wear => {
                let ground = Ground {
                    coarse,
                    reading: self.reading(coarse, parent)?,
                    step: self.step,
                    band: self.band,
                    own: Some((self.square, &self.before)),
                };
                let wearing = self.wearing.as_mut().ok_or(Error::Shape)?;
                if wearing.step((&self.tiles, &ground), store, runner)? {
                    (self.heights, self.flux) = wearing.finish(store)?;
                    self.wearing = None;
                    Stage::Settle(0)
                } else {
                    Stage::Wear
                }
            }
            Stage::Settle(row) => self.settle(coarse, row, runner)?,
            Stage::Done => Stage::Done,
        };
        Ok(self.stage == Stage::Done)
    }

    /// How the parent is read, after level 0: `Shape` for a parent that does
    /// not hold what the piece reads, or that carries nothing where snow lies.
    fn reading<'a>(
        &self,
        coarse: &Coarse,
        parent: Option<Parent<'a>>,
    ) -> Result<Option<Reading<'a>>, Error> {
        match (self.reads, parent) {
            (None, None) => Ok(None),
            (Some(reads), Some(parent)) => {
                let area = parent.square.area()?;
                let bare = parent.attributes.is_empty();
                if !parent.square.holds(reads)
                    || parent.heights.len() != area
                    || !(bare || parent.attributes.len() == area)
                    || (bare && coarse.character.snowpack.is_some())
                {
                    return Err(Error::Shape);
                }
                let ratio = f64::from(coarse.lattices.ratio(self.level)?);
                Ok(Some(Reading {
                    parent,
                    domain: coarse.lattices.domain(self.level - 1)?,
                    ratio,
                    step: self.step * ratio,
                }))
            }
            _ => Err(Error::Shape),
        }
    }

    /// Its own vertices' columns and rows.
    fn own_size(&self) -> Result<(usize, usize), Error> {
        let whole = |value: u32| usize::try_from(value).map_err(|_| Error::Shape);
        Ok((whole(self.square.size.0)?, whole(self.square.size.1)?))
    }

    /// A unit of its own vertices' ground from the vertex numbered `first`:
    /// what each carries, and its height before the droplets.
    fn own(
        &mut self,
        coarse: &Coarse,
        parent: Option<Parent<'_>>,
        first: usize,
        runner: &dyn JobRunner,
    ) -> Result<Stage, Error> {
        let ground = Ground {
            coarse,
            reading: self.reading(coarse, parent)?,
            step: self.step,
            band: self.band,
            own: None,
        };
        let (across, _) = self.own_size()?;
        let total = self.before.len();
        let end = unit_end(first, UNIT_VERTICES, runner).min(total);
        let carried = self.attributes.get_mut(first..end).unwrap_or_default();
        let runs = self
            .before
            .get_mut(first..end)
            .ok_or(Error::Shape)?
            .chunks_mut(UNIT_VERTICES)
            .zip(carried_runs(carried, UNIT_VERTICES))
            .enumerate()
            .map(|(run, (before, attributes))| {
                let filled: Filled<'_> = (first + run * UNIT_VERTICES, attributes, before);
                filled
            });
        let square = self.square;
        tairix_parallel::for_each_drawn(runner, runs, &|(at, mut attributes, before)| {
            for (offset, place) in run_places(square, across, (at, before.len())) {
                let (height, lie) = ground.at(place);
                if let Some(carried) = attributes
                    .as_deref_mut()
                    .and_then(|run| run.get_mut(offset))
                {
                    *carried = coarse.attributes(&lie, height);
                }
                if let Some(was) = before.get_mut(offset) {
                    *was = single(height);
                }
            }
        });
        Ok(if end < total {
            Stage::Own(end)
        } else {
            Stage::Wear
        })
    }

    /// A unit of the piece's own vertices from the one numbered `first`
    /// settled after the droplets: its channels and ways put back to their
    /// beds, on level 0 what the droplets left in each vertex, and its clearing
    /// and cuttings held.
    fn settle(
        &mut self,
        coarse: &Coarse,
        first: usize,
        runner: &dyn JobRunner,
    ) -> Result<Stage, Error> {
        let (across, _) = self.own_size()?;
        let total = self.heights.len();
        let end = unit_end(first, UNIT_VERTICES, runner).min(total);
        let flux = &self.flux;
        let carried = self.attributes.get_mut(first..end).unwrap_or_default();
        let runs = self
            .heights
            .get_mut(first..end)
            .ok_or(Error::Shape)?
            .chunks_mut(UNIT_VERTICES)
            .zip(carried_runs(carried, UNIT_VERTICES))
            .zip(
                self.before
                    .get(first..end)
                    .ok_or(Error::Shape)?
                    .chunks(UNIT_VERTICES),
            )
            .enumerate()
            .map(|(run, ((heights, attributes), before))| {
                let at = first + run * UNIT_VERTICES;
                let water = flux.get(at..at + heights.len()).unwrap_or_default();
                let settled: Settled<'_> = (at, heights, attributes, before, water);
                settled
            });
        let (square, step) = (self.square, self.step);
        let level_zero = self.level == 0;
        tairix_parallel::for_each_drawn(runner, runs, &|(
            at,
            heights,
            mut attributes,
            before,
            water,
        )| {
            for (offset, (x, z)) in run_places(square, across, (at, heights.len())) {
                let (Some(slot), Some(&was)) = (heights.get_mut(offset), before.get(offset)) else {
                    continue;
                };
                let place = (lattice_real(x) * step, lattice_real(z) * step);
                coarse.keep_channel(slot, was, (place, step));
                let carried = attributes
                    .as_deref_mut()
                    .and_then(|run| run.get_mut(offset));
                if let (true, Some(carried)) = (level_zero, carried) {
                    let flux = water.get(offset).copied().unwrap_or(0.0);
                    settled(carried, (was, *slot), flux, coarse.grain(place).roughness);
                }
                coarse.hold(slot, place);
            }
        });
        Ok(if end < total {
            Stage::Settle(end)
        } else {
            Stage::Done
        })
    }

    /// What the whole refinement costs, its items times the measured cost of
    /// one: the scale `done` is a share of, to weigh it against other work.
    #[must_use]
    pub fn cost(&self) -> f64 {
        costs(self.work, real(self.before.len()))
    }

    /// How far the refinement has come, as a share of its work.
    #[must_use]
    pub fn done(&self) -> f64 {
        let own = real(self.before.len());
        let vertices = |first: usize| share(first, self.before.len());
        let worn =
            |work: Work| real_count(work.filled) * FILL_NS + real_count(work.droplets) * DROPLET_NS;
        let spent = match self.stage {
            Stage::Own(first) => own * FILL_NS * vertices(first),
            Stage::Wear => {
                let done = self
                    .wearing
                    .as_ref()
                    .map_or(self.work, |wearing| wearing.work().1);
                own * FILL_NS + worn(done)
            }
            Stage::Settle(first) => {
                own * FILL_NS + worn(self.work) + own * SETTLE_NS * vertices(first)
            }
            Stage::Done => return 1.0,
        };
        mathf::fmin(spent / mathf::fmax(self.cost(), 1.0), 1.0)
    }

    /// The piece, once refined; `Shape` before it is.
    pub fn finish(self) -> Result<Piece, Error> {
        if self.stage != Stage::Done {
            return Err(Error::Shape);
        }
        Ok(Piece {
            level: self.level,
            square: self.square,
            heights: self.heights,
            attributes: self.attributes,
        })
    }
}

/// What a level-0 vertex carries once its droplets have run, from its height
/// `before` them and `now`, and the water `flux` they carried across it: what
/// they wore away or laid down, and where their water ran.
fn settled(attributes: &mut Attributes, (before, now): (f32, f32), flux: f32, roughness: f64) {
    let change = f64::from(now - before) / (0.2 * mathf::fmax(roughness, 0.5));
    let runs = (f64::from(flux) / 12.0).clamp(0.0, 1.0);
    let sediment = attributes::sediment_of(f64::from(attributes[SEDIMENT]) / 255.0);
    attributes[SEDIMENT] = byte(attributes::sediment_share(
        (sediment + change).clamp(-1.0, 1.0),
    ));
    let wet = f64::from(attributes[WET]) / 255.0;
    attributes[WET] = byte(mathf::fmax(wet, 0.8 * runs));
    // Fresh wash and gullies grow less for a while.
    let green = f64::from(attributes[GREEN]) / 255.0;
    attributes[GREEN] = byte(green * (1.0 - 0.5 * smoothstep(0.3, 1.0, change.abs())));
}

/// Droplets for a level whose vertices are `step` apart.
fn droplets(step: f64) -> Droplets {
    Droplets {
        lifetime: 90,
        inertia: 0.08,
        capacity: 4.0,
        min_slope: 0.01 * step,
        erosion: 0.25,
        deposition: 0.25,
        evaporation: 0.018,
        gravity: 4.0 / mathf::fmax(step, 0.1),
        friction: 0.06,
        radius: 3,
    }
}

/// The key level `level`'s droplets are drawn under, on a land built under
/// `seed`.
fn droplet_key(seed: u32, level: usize) -> u64 {
    match level.checked_sub(1) {
        None => u64::from(seed) ^ 0xd50b,
        Some(finer) => numbered(
            u64::from(seed) ^ 0x2ea7,
            u64::try_from(finer).unwrap_or(u64::MAX),
        ),
    }
}

/// How many octaves of detail span wavelengths from `longest` down to four
/// of a level's `step`s: a shorter wave the level holds too few samples of,
/// and the finer levels refining it would draw it out as a ripple running
/// along its own cells. None where even `longest` is too short for it, so
/// the band starts at the first level that holds it.
fn octaves_between(longest: f64, step: f64) -> u32 {
    let held = longest / (4.0 * step);
    if held.is_nan() || held < 1.0 {
        return 0;
    }
    let span = 1.0 + mathf::ln(held) / core::f64::consts::LN_2;
    u32::try_from(mathf::round_i32(mathf::floor(span)))
        .unwrap_or(1)
        .clamp(1, 6)
}

/// The Catmull–Rom patch of the lattice `at` reads, `across` and `down` it in
/// its steps: its value, and its rate of change each way.
pub(crate) fn catmull_rom_2d(
    at: impl Fn(i64, i64) -> f64,
    (across, down): (f64, f64),
) -> (f64, f64, f64) {
    let (west, south) = (mathf::floor(across), mathf::floor(down));
    let (right, lower) = (across - west, down - south);
    let (column, row) = (
        i64::from(mathf::round_i32(west)),
        i64::from(mathf::round_i32(south)),
    );
    // The spline through four samples, and its derivative, at `t`.
    let spline = |[p0, p1, p2, p3]: [f64; 4], t: f64| {
        let (a, b, c) = (
            p2 - p0,
            2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3,
            3.0 * p1 - p0 - 3.0 * p2 + p3,
        );
        (
            f64::midpoint(2.0 * p1, t * (a + t * (b + t * c))),
            f64::midpoint(a, t * (2.0 * b + 3.0 * t * c)),
        )
    };
    let mut rows = [0.0; 4];
    let mut slopes = [0.0; 4];
    for (index, near) in (row - 1..=row + 2).enumerate() {
        (rows[index], slopes[index]) = spline(
            [
                at(column - 1, near),
                at(column, near),
                at(column + 1, near),
                at(column + 2, near),
            ],
            right,
        );
    }
    let (value, down_rate) = spline(rows, lower);
    let (across_rate, _) = spline(slopes, lower);
    (value, across_rate, down_rate)
}

/// A parent level as a piece of the level after it reads it: its held
/// square, which holds all of what the piece reads within the land, and
/// beyond the land's edge each read held to it.
struct Reading<'a> {
    parent: Parent<'a>,
    /// The parent level's lattice over the land.
    domain: Placed,
    /// How many of the child's steps span one of the parent's, and the
    /// parent's step.
    ratio: f64,
    step: f64,
}

impl Reading<'_> {
    /// The parent's height at its vertex `(column, row)`, held to the land.
    fn height(&self, column: i64, row: i64) -> f64 {
        self.vertex(column, row)
            .and_then(|index| self.parent.heights.get(index))
            .map_or(0.0, |&height| f64::from(height))
    }

    /// The index within the parent's held square of its vertex `(column,
    /// row)`, held to the land.
    fn vertex(&self, column: i64, row: i64) -> Option<usize> {
        let held =
            |at: i64, origin: i64, size: u32| (at - origin).clamp(0, i64::from(size) - 1) + origin;
        self.parent.square.index((
            held(column, self.domain.origin.0, self.domain.size.0),
            held(row, self.domain.origin.1, self.domain.size.1),
        ))
    }

    /// Where the child's vertex `at` lies on the parent's lattice.
    fn place(&self, (column, row): (i64, i64)) -> (f64, f64) {
        (
            lattice_real(column) / self.ratio,
            lattice_real(row) / self.ratio,
        )
    }

    /// The parent read through a Catmull–Rom patch at the child's vertex
    /// `at`: its height, and its slope along x and z.
    fn cubic(&self, at: (i64, i64)) -> (f64, f64, f64) {
        let (height, du, dv) =
            catmull_rom_2d(|column, row| self.height(column, row), self.place(at));
        (height, du / self.step, dv / self.step)
    }

    /// The parent's quantities at the child's vertex `at`, blended between
    /// the parent's vertices about it.
    fn quantities(&self, at: (i64, i64)) -> [f64; QUANTITIES] {
        if self.parent.attributes.is_empty() {
            return attributes::PLAIN;
        }
        let (u, v) = self.place(at);
        // The cell a parent coordinate falls in and how far across it, held
        // to the land: its first cell's start below it, its last's end past
        // it.
        let split = |value: f64, origin: i64, size: u32| {
            let cells = i64::from(size) - 1;
            let whole = mathf::floor(value);
            let cell = i64::from(mathf::round_i32(whole)) - origin;
            if cell < 0 {
                (origin, 0.0)
            } else if cell >= cells {
                (origin + (cells - 1).max(0), 1.0)
            } else {
                (origin + cell, (value - whole).clamp(0.0, 1.0))
            }
        };
        let ((column, across), (row, down)) = (
            split(u, self.domain.origin.0, self.domain.size.0),
            split(v, self.domain.origin.1, self.domain.size.1),
        );
        let corner = |dx: i64, dz: i64| {
            self.vertex(column + dx, row + dz)
                .and_then(|index| self.parent.attributes.get(index))
                .copied()
                .unwrap_or([0; attributes::CHANNELS])
        };
        let corners = [corner(0, 0), corner(1, 0), corner(0, 1), corner(1, 1)];
        core::array::from_fn(|channel| attributes::blended(corners, channel, (across, down)))
    }
}

/// A lattice index as a real number: exact, every index a land is built on
/// held by an `i32` (`Lattices::domain`).
fn lattice_real(index: i64) -> f64 {
    f64::from(i32::try_from(index).unwrap_or(if index < 0 { i32::MIN } else { i32::MAX }))
}

#[cfg(test)]
#[path = "level_tests.rs"]
mod tests;

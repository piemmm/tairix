//! A farmed land's boundaries as they stand about the eye:
//!
//! - hedges of thorn and hazel, an oak standing in them now and then and a
//!   collapsed stretch mended with post and rail;
//! - dry-stone walls of field stones ([`drystone`]);
//! - fences of posts and rails, true where they are kept and leaning, mossed
//!   and missing rails where not;
//! - the gates hung in their gateways, shut, open or off a hinge, and the
//!   stiles its paths cross by;
//! - the trees its woodlots are planted with, and its orchards' apple trees
//!   in their rows;
//! - and its cut fields' bales ([`bales`]) and its maize standing as plants
//!   about the eye ([`maize`]).
//!
//! They are set out a bounded unit at a time ([`Fielding`]). Each boundary
//! draws from its own key, so how many a detail sets out never changes what
//! the rest of a scene draws. Beyond the reach hedges and walls stand built
//! they stand on as one mesh ([`far`]) as far as they span a pixel, and
//! their foot is painted on the ground where they run.

use alloc::vec::Vec;
use core::f64::consts::FRAC_PI_2;

use tairix_countryside::boundary::{standing, Boundary, Gap, Kind as Bound, Side, Through};
use tairix_countryside::field::FieldId;
use tairix_countryside::layout::Layout;
use tairix_countryside::plane::{self, Walk};
use tairix_countryside::usage::Use;
use tairix_countryside::{self as countryside, Point};
use tairix_parallel::{for_each, JobRunner};
use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::mathf;

use super::courses::{Dressing, Mason, Quarry, Weathering};
use super::landscape::{ground_of, Vantage};
use super::plants::{self, Grove, Kind, Stand};
use super::woodland::ACROSS;
use super::{Dice, Stage};
use crate::course::{traced, Courses, Indexing, Mark, Reach, INDEX_UNIT};
use crate::detail::Bounds as Reaches;
use crate::farmed::Grown;
use crate::fracture::{running, tear, Break, Grain};
use crate::ground::Bounds as Painted;
use crate::heightfield::Heightfield;
use crate::land::Land;
use crate::noise::noise2;
use crate::sample::mix64;
use crate::solid::Form;
use crate::tree::Season;
use crate::vector::{share, wrapped, Frame, Pose, Vec3};
use crate::wood::{rooted, Habit};

mod bales;
mod drystone;
mod far;
mod growing;
pub(super) mod maize;
mod pats;
mod vines;

use bales::Baling;
use drystone::Walling;

/// How a hedge is planted: thorn the most, hazel among it; how far either
/// side of its line its shrubs are set, as shares of its breadth; and how
/// their height swells and thins along it about its own.
const HAWTHORN: f64 = 0.68;
const ROWED: (f64, f64) = (0.08, 0.3);
const SWELL: (f64, f64) = (0.78, 0.4);

/// How many of a land's hedges have trees standing in them.
const STOOD: f64 = 0.4;

/// How much of each kind of work a unit of the setting out takes: the stones
/// each core's wall lays, the plants and timber set one after another, and
/// the places of a woodlot's lattice looked at.
const STONES_A_UNIT: usize = 500;
const PIECES_A_UNIT: usize = 1500;
const PLACES_A_UNIT: usize = 3000;

/// A farmed land's boundaries being set out about the eye, a bounded unit at
/// a time: its woodlots' and orchards' trees; its cut fields' bales and its
/// maize and vineyards stood as plants; then its boundaries nearest first,
/// the walls among them laid ahead across the runner and every boundary's work handed
/// on in that order, so a scene comes out the same however its work is
/// shared; then its hedges and walls beyond where they stand built, as one
/// mesh, and their foot painted on the ground; and its stonework and timber
/// raised.
#[derive(Debug)]
pub(super) struct Fielding {
    eye: Point,
    heading: f64,
    reach: Reaches,
    /// How far from the eye its walls are laid stone by stone.
    stones: f64,
    /// The trees of its woodlots, the shrubs its hedges are planted with, and
    /// the tree that stands in them.
    trees: Vec<Habit>,
    shrubs: [Habit; 2],
    standard: Option<Habit>,
    /// How many objects its woodlots' and hedges' plants may still take.
    room: usize,
    /// The boundaries near enough to set out, nearest first, by index among
    /// the layout's.
    near: Vec<(f64, usize)>,
    /// Its stonework, its kept timber, and the timber of what is let go.
    stone: Option<Mason>,
    timber: Option<Mason>,
    neglected: Option<Mason>,
    /// The bales its cut fields are left lying in; the seed its fields'
    /// standing crops and vines and its orchards' trees are drawn under; the
    /// season they stand in; and its orchards' apple trees, grown as first
    /// wanted.
    baling: Option<Baling>,
    crops: u64,
    season: Season,
    apple: Option<Habit>,
    /// The materials its hedges and its walls are in far off.
    distant: (u16, u16),
    /// What its stonework, each of its timbers and its bales are raised
    /// keyed under.
    keys: [u32; 4],
    phase: Phase,
}

/// Where a farmed land's setting out stands.
#[allow(
    clippy::large_enum_variant,
    reason = "a setting out holds one phase, and a box could not fail gracefully"
)]
#[derive(Debug)]
enum Phase {
    /// The woodlots and orchards from `parcel` among the layout's, the one in
    /// hand being planted.
    Woodlots {
        parcel: usize,
        lot: Option<Lot>,
    },
    /// The cut fields' bales being laid.
    Bales,
    /// The maize fields and vineyards about the eye being stood as plants.
    Crops,
    /// The near boundaries from `next`, the walls among them being laid ahead
    /// of it from those before `ahead`, and the hedge in hand being planted.
    Boundaries {
        next: usize,
        ahead: usize,
        walls: Vec<(usize, Walling)>,
        hedging: Option<Hedging>,
    },
    /// The hedges and walls beyond where they stand built.
    Distant(far::Distant),
    /// What shows of the hedges where they are not built, then of the walls:
    /// the one being indexed, and the hedges' once they are.
    Painting {
        indexing: Indexing,
        hedges: Option<Courses>,
    },
    /// The stonework raised, then each timber.
    Raising,
    Done,
}

impl Fielding {
    /// The setting out of the countryside `layout` about `vantage` in
    /// `season`, an oak of `grove`'s standing in its hedges now and then and
    /// its woodlots stood with `grove`'s trees; `None` when the heap will not
    /// hold it.
    pub(super) fn new(
        stage: &mut Stage,
        dice: &mut Dice,
        layout: &Layout,
        (vantage, season, grove): (&Vantage, Season, &Grove),
    ) -> Option<Self> {
        let standard = grove
            .of(Kind::Oak)
            .or_else(|| grove.first())
            .map(|grown| grown.habit);
        let reach = stage.densities.bounds;
        let eye = Point::new(vantage.eye.x, vantage.eye.z);
        let hedging = Grove::new(
            stage,
            dice,
            (&[Kind::Hawthorn, Kind::Hazel], season),
            Stand::Open,
        )?;
        let shrubs = [
            hedging.of(Kind::Hawthorn)?.habit,
            hedging.of(Kind::Hazel)?.habit,
        ];
        let quarry = dice.pick(&[Quarry::Limestone, Quarry::Sandstone, Quarry::Granite])?;
        // Field walls and fences run over the whole land, so no one height marks
        // their splashed foot: their damp shows in patches instead. Open to sun
        // and wind they dry quickly, so moss keeps to a few damp patches and
        // lichen has the rest.
        let weathering = Weathering {
            damp: dice.range(0.1, 0.4),
            drought: dice.range(0.1, 0.4),
            foot: -1.0e4,
        };
        let (walled, sawn) = (dice.range(0.6, 0.95), dice.range(0.35, 0.9));
        let stonework = stage.fieldwork(dice, quarry, (walled, weathering))?;
        let stone = Mason::new(stonework, dice.seed())?;
        let timberwork = stage.timberwork(dice, (sawn, weathering), None)?;
        let timber = Mason::new(timberwork, dice.seed())?;
        // What is let go stands long unmended: silvered, and mossed where it
        // stays damp.
        let neglect = Weathering {
            damp: (weathering.damp + 0.35).min(1.0),
            ..weathering
        };
        let letgo = stage.timberwork(dice, ((sawn + 0.3).min(1.0), neglect), None)?;
        let neglected = Mason::new(letgo, dice.seed())?;
        for work in [stonework, timberwork, letgo] {
            stage.snow_on(work, plants::snowed(season))?;
        }
        let distant = (
            far::canopy(stage, season, dice.seed())?,
            far::massed(stage, stonework.stone)?,
        );
        // Drawn whether or not each is ever raised, so the scene's later
        // draws never hang on how much a detail sets out.
        let keys = [dice.seed(), dice.seed(), dice.seed(), dice.seed()];
        let fielded = dice.wide();
        let baling = Baling::new(
            layout,
            (Point::new(vantage.eye.x, vantage.eye.z), vantage.heading),
            fielded,
        )?;
        let stones = drystone::TYPICAL / (reach.stones * stage.pixel);
        let farthest = reach
            .hedges
            .max(stones)
            .max(reach.fences)
            .max(reach.standards);
        let mut near = Vec::new();
        for (index, boundary) in layout.boundaries().iter().enumerate() {
            let apart =
                plane::nearest(&boundary.line, eye).map_or(f64::INFINITY, |near| near.distance);
            if apart < farthest {
                near.try_reserve(1).ok()?;
                near.push((apart, index));
            }
        }
        let boundaries = layout.boundaries();
        near.sort_unstable_by(|a, b| {
            a.0.total_cmp(&b.0)
                .then(boundaries[a.1].id.cmp(&boundaries[b.1].id))
        });
        let mut trees = Vec::new();
        trees.try_reserve_exact(grove.kinds().count()).ok()?;
        trees.extend(grove.kinds().map(|grown| grown.habit));
        Some(Self {
            eye,
            heading: vantage.heading,
            reach,
            stones,
            trees,
            shrubs,
            standard,
            room: budget(stage.room(), reach.share),
            near,
            stone: Some(stone),
            timber: Some(timber),
            neglected: Some(neglected),
            baling: Some(baling),
            crops: mix64(fielded ^ 0x006d_6169_7a65),
            season,
            apple: None,
            distant,
            keys,
            phase: Phase::Woodlots {
                parcel: 0,
                lot: None,
            },
        })
    }

    /// The seed its pastures' pats are scattered under, which its sward
    /// stands rank about.
    pub(super) fn grazing(&self) -> u32 {
        Dice::keyed(mix64(self.crops ^ 0x6772), 0).seed()
    }

    /// The apple trees its orchards are planted with, grown the first time
    /// from draws of their own; `None` when the stage will not hold them.
    fn apple(&mut self, stage: &mut Stage) -> Option<Habit> {
        if let Some(apple) = self.apple {
            return Some(apple);
        }
        let mut dice = Dice::keyed(mix64(self.crops ^ 0x6170), 0);
        let apple = Grove::new(stage, &mut dice, (&[Kind::Apple], self.season), Stand::Open)?
            .of(Kind::Apple)?
            .habit;
        self.apple = Some(apple);
        Some(apple)
    }

    /// Where its far hedges and walls are set out from, at `pixel` wide a
    /// metre off.
    fn sight(&self, pixel: f64) -> far::Sight {
        far::Sight {
            eye: self.eye,
            heading: self.heading,
            pixel,
            built: (self.reach.hedges, self.stones),
            materials: self.distant,
            spread: far::spread(),
        }
    }

    /// How far the setting out has come, as a share: the boundaries most of
    /// it, laid nearest first.
    pub(super) fn done(&self) -> f64 {
        match &self.phase {
            Phase::Woodlots { .. } => 0.0,
            Phase::Bales => 0.02 + 0.03 * self.baling.as_ref().map_or(1.0, Baling::done),
            Phase::Crops => 0.05,
            Phase::Boundaries { next, .. } => 0.05 + 0.8 * share(*next, self.near.len()),
            Phase::Distant(distant) => 0.85 + 0.05 * distant.done(),
            Phase::Painting { hedges, .. } => 0.9 + if hedges.is_some() { 0.04 } else { 0.0 },
            Phase::Raising => 0.98,
            Phase::Done => 1.0,
        }
    }

    /// The next unit of the setting out of `land` on `stage` across `runner`;
    /// whether it is whole, or `None` when the heap will not hold it.
    pub(super) fn step(
        &mut self,
        stage: &mut Stage,
        (land, runner): (&Land, &dyn JobRunner),
    ) -> Option<bool> {
        let layout = land.layout.as_ref()?;
        let phase = core::mem::replace(&mut self.phase, Phase::Done);
        self.phase = match phase {
            Phase::Woodlots {
                mut parcel,
                mut lot,
            } => {
                if self.woodlots(stage, (land, layout), (&mut parcel, &mut lot))? {
                    Phase::Bales
                } else {
                    Phase::Woodlots { parcel, lot }
                }
            }
            Phase::Bales => {
                let baled = match self.baling.as_mut() {
                    Some(baling) => baling.step(stage, (land, layout))?,
                    None => true,
                };
                if baled {
                    Phase::Crops
                } else {
                    Phase::Bales
                }
            }
            Phase::Crops => {
                self.crops(stage, (land, layout))?;
                Phase::Boundaries {
                    next: 0,
                    ahead: 0,
                    walls: Vec::new(),
                    hedging: None,
                }
            }
            Phase::Boundaries {
                mut next,
                mut ahead,
                mut walls,
                mut hedging,
            } => {
                if self.boundaries(
                    stage,
                    (land, layout, runner),
                    (&mut next, &mut ahead),
                    (&mut walls, &mut hedging),
                )? {
                    Phase::Distant(far::Distant::new(
                        self.sight(stage.pixel),
                        layout.boundaries().len(),
                    ))
                } else {
                    Phase::Boundaries {
                        next,
                        ahead,
                        walls,
                        hedging,
                    }
                }
            }
            Phase::Distant(mut distant) => {
                if distant.step(stage, land)? {
                    Phase::Painting {
                        indexing: painted(layout.boundaries(), land, Bound::Hedge)?,
                        hedges: None,
                    }
                } else {
                    Phase::Distant(distant)
                }
            }
            Phase::Painting {
                mut indexing,
                hedges,
            } => {
                if !indexing.step(INDEX_UNIT)? {
                    Phase::Painting { indexing, hedges }
                } else if let Some(hedges) = hedges {
                    let walls = indexing.finish()?;
                    if let Some(ground) = ground_of(stage, land) {
                        ground.bounds = Some(Painted { hedges, walls });
                    }
                    Phase::Raising
                } else {
                    Phase::Painting {
                        hedges: Some(indexing.finish()?),
                        indexing: painted(layout.boundaries(), land, Bound::Wall)?,
                    }
                }
            }
            Phase::Raising => {
                self.raise(stage)?;
                Phase::Done
            }
            Phase::Done => return Some(true),
        };
        Some(matches!(self.phase, Phase::Done))
    }

    /// Stand the crops of `layout` near the eye on `land`: its maize and its
    /// vines, and the pats its stock leave.
    fn crops(&self, stage: &mut Stage, (land, layout): (&Land, &Layout)) -> Option<()> {
        maize::stand(
            stage,
            (land, layout),
            self.eye,
            (maize::standing(&self.reach), self.crops),
        )?;
        vines::stand(
            stage,
            (land, layout),
            (self.eye, self.season),
            (self.reach.vines, mix64(self.crops ^ 0x76)),
        )?;
        pats::lay(stage, land, (self.eye, self.reach.pats), self.grazing())?;
        Some(())
    }

    /// Raise on `stage` what its masons built and its bales.
    fn raise(&mut self, stage: &mut Stage) -> Option<()> {
        let masons = [self.stone.take(), self.timber.take(), self.neglected.take()];
        for (mason, key) in masons.into_iter().zip(self.keys) {
            if let Some(mason) = mason.filter(|mason| !mason.is_empty()) {
                stage.raise(mason, Pose::new(Vec3::ZERO, Frame::WORLD), key)?;
            }
        }
        if let Some(baling) = self.baling.take() {
            baling.raise(stage, self.keys[3])?;
        }
        Some(())
    }

    /// A unit of the woodlots of `layout` from `parcel`, the one in hand
    /// being `lot`; whether every woodlot is planted.
    fn woodlots(
        &mut self,
        stage: &mut Stage,
        (land, layout): (&Land, &Layout),
        (parcel, lot): (&mut usize, &mut Option<Lot>),
    ) -> Option<bool> {
        let mut looked = 0;
        while looked < PLACES_A_UNIT {
            let Some(planting) = lot.as_mut() else {
                let Some(next) = layout.parcels().get(*parcel) else {
                    return Some(true);
                };
                *lot = Lot::new(next, (self.eye, self.reach.standards));
                if lot.is_none() {
                    *parcel += 1;
                }
                continue;
            };
            let (done, spent) =
                planting.plant(stage, (land, layout), (self, PLACES_A_UNIT - looked))?;
            looked += spent;
            if done {
                *lot = None;
                *parcel += 1;
            }
        }
        Some(false)
    }

    /// A unit of the near boundaries of `layout` from `next`: the walls
    /// among them, begun in their order from those before `ahead`, laid on
    /// across `runner`, a wall to each core; then each boundary's work in its
    /// order, as far as the unit's pieces last or a wall not yet laid; whether
    /// every one is set out.
    fn boundaries(
        &mut self,
        stage: &mut Stage,
        (land, layout, runner): (&Land, &Layout, &dyn JobRunner),
        (next, ahead): (&mut usize, &mut usize),
        (walls, hedging): (&mut Vec<(usize, Walling)>, &mut Option<Hedging>),
    ) -> Option<bool> {
        let mut finished = self.lay_walls(stage, (land, layout, runner), ahead, walls)?;
        let boundaries = layout.boundaries();
        let mut pieces = 0;
        while pieces < PIECES_A_UNIT {
            let Some(&(apart, index)) = self.near.get(*next) else {
                return Some(true);
            };
            let boundary = boundaries.get(index)?;
            let edge = Laid {
                land,
                line: &boundary.line,
                eye: self.eye,
            };
            match boundary.kind {
                Bound::Wall if apart < self.stones => {
                    // The wall at hand is always the first begun, handed to the
                    // mason as far as the unit's pieces last once it is laid.
                    let (Some((at, walling)), Some(&whole)) = (walls.first_mut(), finished.first())
                    else {
                        return Some(false);
                    };
                    if *at != *next || !whole {
                        return Some(false);
                    }
                    let stone = self.stone.as_mut()?;
                    while walling.committed < walling.units.len() {
                        if pieces >= PIECES_A_UNIT {
                            return Some(false);
                        }
                        let unit = walling.units[walling.committed];
                        stone.unit(unit.placing, unit.form, unit.dressing)?;
                        walling.committed += 1;
                        pieces += 1;
                    }
                    claim(stage, edge.line, walling.foot())?;
                    finished.remove(0);
                    walls.remove(0);
                }
                Bound::Hedge if apart < self.reach.hedges => {
                    let planting = match hedging.as_mut() {
                        Some(planting) => planting,
                        None => hedging.insert(Hedging::new(boundary)),
                    };
                    let (done, spent) =
                        planting.plant(stage, (&edge, boundary), (self, PIECES_A_UNIT - pieces))?;
                    pieces += spent;
                    if !done {
                        return Some(false);
                    }
                    *hedging = None;
                }
                _ => pieces += self.set_out_plainly(stage, (&edge, boundary), apart)?,
            }
            pieces += self.hang_gaps(stage, (&edge, boundary), apart)?;
            *next += 1;
        }
        Some(false)
    }

    /// Begin the walls among the near boundaries from `ahead`, in their order,
    /// until `walls` holds as many as `runner` is wide, and lay each on a
    /// unit, a wall to each core; whether each is laid.
    fn lay_walls(
        &self,
        stage: &Stage,
        (land, layout, runner): (&Land, &Layout, &dyn JobRunner),
        ahead: &mut usize,
        walls: &mut Vec<(usize, Walling)>,
    ) -> Option<Vec<bool>> {
        let boundaries = layout.boundaries();
        while walls.len() < runner.width().max(1) {
            let Some(&(apart, index)) = self.near.get(*ahead) else {
                break;
            };
            if let Some(boundary) = boundaries
                .get(index)
                .filter(|boundary| boundary.kind == Bound::Wall && apart < self.stones)
            {
                let mut draws = Dice::keyed(boundary.key, 0);
                walls.try_reserve(1).ok()?;
                walls.push((*ahead, Walling::new(boundary, &mut draws)?));
            }
            *ahead += 1;
        }
        let (eye, reach, fields) = (self.eye, self.stones, &stage.fields[..]);
        // Each wall's step answers whether it is laid, or that the heap
        // refused it, which leaves it unlaid.
        let mut jobs: Vec<(&mut Walling, &Boundary, Stepped)> = Vec::new();
        jobs.try_reserve_exact(walls.len()).ok()?;
        for (at, walling) in walls.iter_mut() {
            jobs.push((
                walling,
                boundaries.get(self.near.get(*at)?.1)?,
                Stepped::Waiting,
            ));
        }
        for_each(runner, &mut jobs, &|(walling, boundary, stepped)| {
            let edge = Laid {
                land,
                line: &boundary.line,
                eye,
            };
            *stepped = match walling.step((&edge, fields), (reach, STONES_A_UNIT)) {
                Some(true) => Stepped::Laid,
                Some(false) => Stepped::Waiting,
                None => Stepped::Refused,
            };
        });
        let mut finished = Vec::new();
        finished.try_reserve_exact(jobs.len()).ok()?;
        for (.., stepped) in &jobs {
            finished.push(match stepped {
                Stepped::Laid => true,
                Stepped::Waiting => false,
                Stepped::Refused => return None,
            });
        }
        Some(finished)
    }

    /// Set out `edge`'s boundary, `boundary`, `apart` from the eye, where it
    /// is a hedge too far off to plant but whose trees still stand, or a
    /// fence; how many pieces it took.
    fn set_out_plainly(
        &mut self,
        stage: &mut Stage,
        (edge, boundary): (&Laid<'_>, &Boundary),
        apart: f64,
    ) -> Option<usize> {
        match boundary.kind {
            Bound::Hedge if apart < self.reach.standards => match self.standard {
                Some(standard) => {
                    let mut draws = Dice::keyed(boundary.key, 0);
                    standards(
                        stage,
                        edge,
                        &standard,
                        (boundary, &mut draws),
                        &mut self.room,
                    )
                }
                None => Some(0),
            },
            Bound::Fence if apart < self.reach.fences => {
                let mut draws = Dice::keyed(boundary.key, 0);
                let kept = draws.chance(0.6);
                let length = plane::length(&boundary.line);
                let reach = self.reach.fences;
                let fenced = ((0.0, length), &boundary.gaps[..], (kept, boundary.height));
                fence(
                    (stage, edge),
                    self.timbered(kept)?,
                    &mut draws,
                    fenced,
                    reach,
                )
            }
            Bound::Hedge | Bound::Wall | Bound::Fence | Bound::Ditch | Bound::Open => Some(0),
        }
    }

    /// Hang `boundary`'s gates and set its stiles where they lie within reach
    /// of the eye, along `edge`, the boundary `apart` from it at its nearest;
    /// how many pieces they took.
    fn hang_gaps(
        &mut self,
        stage: &mut Stage,
        (edge, boundary): (&Laid<'_>, &Boundary),
        apart: f64,
    ) -> Option<usize> {
        if boundary.kind == Bound::Open || apart >= self.reach.fences {
            return Some(0);
        }
        let mut pieces = 0;
        for gap in &boundary.gaps {
            let Some((at, _)) = plane::at(&boundary.line, gap.along) else {
                continue;
            };
            if (at - self.eye).length() > self.reach.fences {
                continue;
            }
            let (timber, neglected, stone) = (
                self.timber.as_mut()?,
                self.neglected.as_mut()?,
                self.stone.as_mut()?,
            );
            pieces += match gap.through {
                Through::Gateway => gate(stage, edge, (boundary, gap), (timber, neglected))?,
                Through::Path => stile(stage, edge, (boundary, gap), (timber, stone))?,
            };
        }
        Some(pieces)
    }

    /// The mason laying its timber where it is `kept`, or else where it is
    /// let go.
    fn timbered(&mut self, kept: bool) -> Option<&mut Mason> {
        if kept {
            self.timber.as_mut()
        } else {
            self.neglected.as_mut()
        }
    }
}

/// What a unit of a wall's laying came to.
#[derive(Copy, Clone, Debug)]
enum Stepped {
    Waiting,
    Laid,
    Refused,
}

/// How many of a scene's `room` objects its hedges may take, at `share`.
fn budget(room: usize, share: f64) -> usize {
    let room = u32::try_from(room).unwrap_or(u32::MAX);
    usize::try_from(mathf::round_i32((f64::from(room) * share).min(2.0e9))).unwrap_or(0)
}

/// A boundary's line over its land, and where the eye stands.
struct Laid<'a> {
    land: &'a Land,
    line: &'a [Point],
    eye: Point,
}

impl Laid<'_> {
    /// The ground's height at `at` on the land's `fields`, beneath any snow.
    fn ground(&self, fields: &[Heightfield], at: Point) -> f64 {
        self.land.grids.beneath_snow(fields, at.x, at.y)
    }

    /// Where a thing `height` tall stands rooted at `at` on the land's
    /// `fields`.
    fn rooted(&self, fields: &[Heightfield], at: Point, height: f64) -> f64 {
        rooted(&self.land.grids.lie(fields, at.x, at.y), height)
    }
}

/// Whether `along` a line lies within `reach` of the middle of any of
/// `gaps`, beyond half its width.
fn in_gap(gaps: &[Gap], along: f64, reach: f64) -> bool {
    gaps.iter()
        .any(|gap| (along - gap.along).abs() < 0.5 * gap.width + reach)
}

/// Places evenly from `from` to `to`, both ends among them, no further apart
/// than `most`.
fn spaced((from, to): (f64, f64), most: f64) -> impl Iterator<Item = f64> {
    let (span, count) = (to - from, divided((from, to), most));
    (0..=count).map(move |index| from + span * f64::from(index) / f64::from(count))
}

/// How many even steps take `from` to `to` none longer than `most`.
fn divided((from, to): (f64, f64), most: f64) -> i32 {
    mathf::round_i32(mathf::ceil((to - from) / most.max(1e-3)).clamp(1.0, 1.0e6)).max(1)
}

/// The frame of a thing laid along the unit way `way` of the land's plan:
/// its own `x` along it, `y` up, `z` across it to its right.
fn along(way: Point) -> Frame {
    Frame::turned(mathf::atan2(-way.y, way.x), 0.0)
}

/// `frame` tipped `angle` radians about its own `z`, raising its `x`.
fn tipped(frame: Frame, angle: f64) -> Frame {
    frame.rotated_by(Frame::about(frame.z, angle))
}

/// `frame` leant `angle` radians about its own `x`.
fn leant(frame: Frame, angle: f64) -> Frame {
    frame.rotated_by(Frame::about(frame.x, angle))
}

/// A hedge being planted along a boundary, a stride at a time: its height,
/// breadth and how far apart the trees standing in it are, where its next
/// tree is due, the stretch of it that collapsed and was mended, the key its
/// height wanders under, where along it the next stride is, and on which side
/// of its line.
#[derive(Debug)]
struct Hedging {
    draws: Dice,
    height: f64,
    breadth: f64,
    spacing: f64,
    next_standard: Option<f64>,
    mended: Option<(f64, f64)>,
    seed: u32,
    along: f64,
    row: f64,
}

impl Hedging {
    /// The hedge `boundary` is to be planted as, as tall as it stands and the
    /// rest drawn from its key.
    fn new(boundary: &Boundary) -> Self {
        let mut draws = Dice::keyed(boundary.key, 0);
        let length = plane::length(&boundary.line);
        let height = boundary.height;
        let breadth = draws.range(1.2, 2.0);
        let spacing = draws.range(40.0, 110.0);
        let next_standard = draws.chance(STOOD).then(|| draws.range(6.0, spacing));
        let mended = draws.chance(0.15).then(|| {
            let span = draws.range(3.0, 6.5).min(0.4 * length);
            let from = draws.range(0.1, 0.9) * (length - span);
            (from, from + span)
        });
        let seed = draws.seed();
        let along = draws.range(0.2, 0.6);
        Self {
            draws,
            height,
            breadth,
            spacing,
            next_standard,
            mended,
            seed,
            along,
            row: 1.0,
        }
    }

    /// How tall the hedge's shrubs stand `along` it: its height wandering,
    /// gappy where it thins.
    fn tall(&self, along: f64) -> f64 {
        self.height * (SWELL.0 + SWELL.1 * noise2(along / 7.0, 0.0, self.seed))
    }

    /// Plant on along `laid`'s line, `boundary`'s, as far as `pieces` more
    /// plants: shrubs of `fielding`'s a stride apart, thorn the most, each
    /// its own height about the hedge's and set a little off its line; a tree
    /// of its standard now and then; and, once it is planted, a fence mended
    /// across where a stretch of it collapsed. Out to the hedges' reach from
    /// the eye, and while the room lasts. Whether it is planted, and how many
    /// pieces it took.
    fn plant(
        &mut self,
        stage: &mut Stage,
        (laid, boundary): (&Laid<'_>, &Boundary),
        (fielding, pieces): (&mut Fielding, usize),
    ) -> Option<(bool, usize)> {
        let length = plane::length(laid.line);
        let reach = fielding.reach.hedges;
        let standard = fielding.standard.filter(|_| self.next_standard.is_some());
        let mut spent = 0;
        let mut walk = Walk::new(laid.line);
        while self.along < length {
            if spent >= pieces {
                return Some((false, spent));
            }
            // Two rows set alternately either side of the line, as a hedge is
            // planted: one thicket, its foot never bare.
            let step = self.draws.range(0.4, 0.62);
            self.row = -self.row;
            let here = self.along;
            self.along += step;
            if in_gap(&boundary.gaps, here, 0.45)
                || self
                    .mended
                    .is_some_and(|(from, to)| (from..to).contains(&here))
            {
                continue;
            }
            let Some((at, way)) = walk.at(here) else {
                continue;
            };
            let apart = (at - laid.eye).length();
            // Its trees stand on beyond its shrubs, as far as standards do.
            if let (Some(standard), Some(due)) = (standard.as_ref(), self.next_standard) {
                if here >= due {
                    self.next_standard = Some(due + self.spacing * self.draws.range(0.7, 1.3));
                    let tall = self.draws.range(0.65, 1.0) * standard.tallest();
                    if apart <= fielding.reach.standards && fielding.room > 0 {
                        plant(stage, laid, standard, (at, tall), &mut self.draws)?;
                        fielding.room = fielding.room.saturating_sub(1);
                        spent += 1;
                    }
                    continue;
                }
            }
            if apart > reach || fielding.room == 0 {
                continue;
            }
            let habit = if self.draws.chance(HAWTHORN) {
                fielding.shrubs[0]
            } else {
                fielding.shrubs[1]
            };
            let offset =
                way.left() * (self.row * self.breadth * self.draws.range(ROWED.0, ROWED.1));
            plant(
                stage,
                laid,
                &habit,
                (at + offset, self.tall(here)),
                &mut self.draws,
            )?;
            fielding.room = fielding.room.saturating_sub(1);
            spent += 1;
        }
        if let Some((from, to)) = self.mended {
            let (low, high) = Bound::Fence.stands();
            let above = self.draws.range(low, high);
            let timber = fielding.timbered(false)?;
            spent += fence(
                (stage, laid),
                timber,
                &mut self.draws,
                ((from, to), &[], (false, above)),
                reach,
            )?;
        }
        claim(stage, laid.line, 0.5 * self.breadth)?;
        Some((true, spent))
    }
}

/// Plant one of `habit` about `height` tall at `at`, turned any way.
fn plant(
    stage: &mut Stage,
    laid: &Laid<'_>,
    habit: &Habit,
    (at, height): (Point, f64),
    draws: &mut Dice,
) -> Option<()> {
    let base = laid.rooted(&stage.fields, at, height);
    plants::plant(stage, draws, habit, Vec3::new(at.x, base, at.y), height)
}

/// The oaks standing in a hedge out beyond where its shrubs are built,
/// where it is painted on the ground: what still stands up out of it. How
/// many were planted.
fn standards(
    stage: &mut Stage,
    laid: &Laid<'_>,
    standard: &Habit,
    (boundary, draws): (&Boundary, &mut Dice),
    room: &mut usize,
) -> Option<usize> {
    let length = plane::length(laid.line);
    if !draws.chance(STOOD) {
        return Some(0);
    }
    let spacing = draws.range(40.0, 110.0);
    let mut along = draws.range(6.0, spacing);
    let mut walk = Walk::new(laid.line);
    let mut planted = 0;
    while along < length && *room > 0 {
        let here = along;
        along += spacing * draws.range(0.7, 1.3);
        if in_gap(&boundary.gaps, here, 2.0) {
            continue;
        }
        let Some((at, _)) = walk.at(here) else {
            continue;
        };
        let tall = draws.range(0.65, 1.0) * standard.tallest();
        plant(stage, laid, standard, (at, tall), draws)?;
        *room = room.saturating_sub(1);
        planted += 1;
    }
    Some(planted)
}

/// A land being set out, as its countryside reads it: its ground as built,
/// and the water on it, to ask which field a place lies in.
struct Built<'a> {
    land: &'a Land,
    fields: &'a [Heightfield],
}

impl countryside::Waters for Built<'_> {
    fn height(&self, at: Point) -> f64 {
        self.land.grids.height(self.fields, at.x, at.y)
    }

    fn water(&self, at: Point) -> Option<f64> {
        let (ground, surface) = (
            self.height(at),
            self.land.grids.surface(self.fields, at.x, at.y),
        );
        (surface > ground).then_some(surface)
    }
}

/// Whether ground grown as `grown` stands about as tall as the eye, walling
/// a lookout in: maize standing as plants, and a vineyard's rows.
pub(in crate::compose) fn tall(grown: Grown) -> bool {
    maize::stands(grown) || grown == Grown::Vineyard
}

/// The key field `id`'s own draws are taken under, folded into a land's
/// seed: what lies on a field draws the same however much of the land is
/// set out.
fn field_key(id: FieldId) -> u64 {
    (u64::from(id.holding.i.unsigned_abs()) << 40)
        ^ (u64::from(id.holding.j.unsigned_abs()) << 20)
        ^ u64::from(id.index)
}

/// Whether `at` lies across the view from an eye at `eye` looking along
/// `heading`, or near enough either side of it to shadow what is seen.
fn across_view((eye, heading): (Point, f64), at: Point) -> bool {
    let apart = at - eye;
    wrapped(mathf::atan2(apart.x, apart.y) - heading).abs() <= ACROSS
}

/// Whether a tree `tall` at `at` would stand so near the eye at `eye`, or so
/// near ahead of it looking along `heading`, that it walls the view off.
fn walls_off(eye: Point, heading: f64, (at, tall): (Point, f64)) -> bool {
    let apart = at - eye;
    let distance = apart.length();
    if distance < 18.0 {
        return true;
    }
    let ahead = Point::new(mathf::sin(heading), mathf::cos(heading));
    let off = mathf::atan2(apart.cross(ahead).abs(), apart.dot(ahead));
    distance < 4.0 * tall && off < 0.6
}

/// A woodlot being planted: a jittered lattice of trees filling its field
/// but for where a tree would wall the view off, its draws, its spacing and
/// stature, the rectangle its field lies in, and the next place of its
/// lattice.
#[derive(Debug)]
struct Lot {
    field: countryside::field::FieldId,
    draws: Dice,
    /// Whether it is an orchard, planted in rows with its apple trees,
    /// rather than a woodlot of the land's own.
    orchard: bool,
    /// Its lattice: its first corner, the unit ways its rows run along and
    /// across, how far apart its trees stand along a row and its rows lie,
    /// how far off its place each stands as a share of that, and how far
    /// it runs along and across.
    origin: Point,
    along: Point,
    across: Point,
    spacing: (f64, f64),
    jitter: f64,
    extent: (f64, f64),
    stature: f64,
    /// How far along and across it the planting has come.
    at: (f64, f64),
}

impl Lot {
    /// The woodlot or orchard `parcel` is to be planted as, where it is one
    /// within `reach` of `eye`.
    fn new(parcel: &countryside::layout::Parcel, (eye, reach): (Point, f64)) -> Option<Self> {
        let orchard = match parcel.usage.used {
            Use::Woodlot => false,
            Use::Orchard => true,
            _ => return None,
        };
        if (parcel.field.middle - eye).length() > reach {
            return None;
        }
        let field = parcel.field.id;
        let mut draws = Dice::keyed(field_key(field), 2);
        if orchard {
            // Its rows run along the field's length.
            let along = parcel.field.along;
            let across = Point::new(-along.y, along.x);
            let (mut low, mut high) = (
                (f64::INFINITY, f64::INFINITY),
                (f64::NEG_INFINITY, f64::NEG_INFINITY),
            );
            for corner in &parcel.field.cell.corners {
                let (a, c) = (corner.dot(along), corner.dot(across));
                low = (low.0.min(a), low.1.min(c));
                high = (high.0.max(a), high.1.max(c));
            }
            let spacing = (draws.range(4.2, 5.2), draws.range(5.6, 6.8));
            return Some(Self {
                field,
                draws,
                orchard,
                origin: along * low.0 + across * low.1,
                along,
                across,
                spacing,
                jitter: 0.03,
                extent: (high.0 - low.0, high.1 - low.1),
                stature: 0.85,
                at: (0.5 * spacing.0, 0.5 * spacing.1),
            });
        }
        let bounds = parcel.field.cell.bounds()?;
        let spacing = draws.range(5.5, 7.5);
        let stature = draws.range(0.6, 0.9);
        Some(Self {
            field,
            draws,
            orchard,
            origin: bounds.low,
            along: Point::new(1.0, 0.0),
            across: Point::new(0.0, 1.0),
            spacing: (spacing, spacing),
            jitter: 0.4,
            extent: (bounds.high.x - bounds.low.x, bounds.high.y - bounds.low.y),
            stature,
            at: (0.5 * spacing, 0.5 * spacing),
        })
    }

    /// Plant on with `fielding`'s trees, or its apple trees in an orchard,
    /// as far as `places` more of the lattice, while its room lasts; whether
    /// it is planted, and how many places it looked at.
    fn plant(
        &mut self,
        stage: &mut Stage,
        (land, layout): (&Land, &Layout),
        (fielding, places): (&mut Fielding, usize),
    ) -> Option<(bool, usize)> {
        let apple = if self.orchard {
            Some(fielding.apple(stage)?)
        } else {
            None
        };
        if apple.is_none() && fielding.trees.is_empty() {
            return Some((true, 0));
        }
        let mut looked = 0;
        while self.at.1 < self.extent.1 && fielding.room > 0 {
            if self.at.0 >= self.extent.0 {
                self.at = (0.5 * self.spacing.0, self.at.1 + self.spacing.1);
                continue;
            }
            if looked >= places {
                return Some((false, looked));
            }
            looked += 1;
            let (along, across) = (
                self.at.0 + self.spacing.0 * self.draws.range(-self.jitter, self.jitter),
                self.at.1 + self.spacing.1 * self.draws.range(-self.jitter, self.jitter),
            );
            let at = self.origin + self.along * along + self.across * across;
            self.at.0 += self.spacing.0;
            let built = Built {
                land,
                fields: &stage.fields,
            };
            if layout
                .parcel_at(at, &built)
                .is_none_or(|here| here.field.id != self.field)
            {
                continue;
            }
            let habit = if let Some(apple) = apple {
                apple
            } else {
                let last = u32::try_from(fielding.trees.len() - 1).ok()?;
                fielding.trees[usize::try_from(self.draws.count(0, last)).ok()?]
            };
            let tall = self.stature * self.draws.range(0.8, 1.1) * habit.tallest();
            let trunk = 0.25 + 0.025 * tall;
            if walls_off(fielding.eye, fielding.heading, (at, tall))
                || !stage.clear((at.x, at.y), trunk)
            {
                continue;
            }
            stage.claim((at.x, at.y), trunk)?;
            let base = rooted(&land.grids.lie(&stage.fields, at.x, at.y), tall);
            plants::plant(
                stage,
                &mut self.draws,
                &habit,
                Vec3::new(at.x, base, at.y),
                tall,
            )?;
            fielding.room = fielding.room.saturating_sub(1);
        }
        Some((true, looked))
    }
}

/// Claim the ground along `line`, `half` either side, so nothing grows in
/// what stands there.
fn claim(stage: &mut Stage, line: &[Point], half: f64) -> Option<()> {
    for pair in line.windows(2) {
        stage.claim_along(
            (pair[0].x, pair[0].y),
            (pair[1].x, pair[1].y),
            half.max(0.3),
        )?;
    }
    Some(())
}

/// A post-and-rail fence along the stretch `from`..`to` of the line, but
/// across `gaps` and out to `reach` from the eye: posts a stride or so apart,
/// standing `above` the ground, sunk into it and leant a little; two or
/// three rails between each pair. One `kept` stands true and whole; one not
/// leans, its rails sag, break or are gone. How many pieces it was built of.
fn fence(
    (stage, laid): (&Stage, &Laid<'_>),
    mason: &mut Mason,
    draws: &mut Dice,
    ((from, to), gaps, (kept, above)): ((f64, f64), &[Gap], (bool, f64)),
    reach: f64,
) -> Option<usize> {
    let rails: &[f64] = if draws.chance(0.55) {
        &[0.42, 0.78, 1.08]
    } else {
        &[0.5, 1.02]
    };
    let stride = draws.range(1.8, 2.5);
    let lean = if kept { 0.012 } else { draws.range(0.04, 0.14) };
    let mut walk = Walk::new(laid.line);
    let mut pieces = 0;
    for stretch in standing(gaps, (from, to), 0.4).ok()? {
        let mut last: Option<(Point, f64)> = None;
        for along in spaced(stretch, stride) {
            let Some((at, _)) = walk.at(along) else {
                continue;
            };
            // Every post and rail draws as it would within reach, so what is
            // built does not hang on how far a detail builds.
            let built = (at - laid.eye).length() <= reach;
            let here = (at, laid.ground(&stage.fields, at));
            post(mason, here, (above, 0.065), (lean, draws), built)?;
            if let Some(before) = last {
                for &height in rails {
                    if kept || !draws.chance(0.18) {
                        rail(mason, (before, here), height, (kept, draws), built)?;
                    }
                }
            }
            pieces += 1 + rails.len();
            last = Some(here);
        }
    }
    Some(pieces)
}

/// A squared post at `at` on ground `ground` high, `above` of it standing
/// over it and as much again below a third of it sunk, `half` its side
/// either way, leant up to `lean` radians any way; laid only where `built`.
fn post(
    mason: &mut Mason,
    (at, ground): (Point, f64),
    (above, half): (f64, f64),
    (lean, draws): (f64, &mut Dice),
    built: bool,
) -> Option<()> {
    let sunk = 0.45 * above;
    let upright = Frame::turned(draws.range(0.0, core::f64::consts::TAU), 0.0);
    let frame = leant(
        tipped(upright, lean * draws.range(-1.0, 1.0)),
        lean * draws.range(-1.0, 1.0),
    );
    if !built {
        return Some(());
    }
    let middle = Vec3::new(at.x, ground, at.y) + frame.y * (0.5 * (above - sunk));
    // A timber unit's grain runs along its own `x`: stood on end, up the post.
    mason.unit(
        (
            Pose::new(middle, tipped(frame, FRAC_PI_2)),
            Vec3::new(f64::midpoint(above, sunk), half, half),
        ),
        Form::Block { fan: 0 },
        Dressing::Timber,
    )
}

/// A rail from post `a` to post `b`, each with the ground's height there,
/// `height` above the ground between them: laid along its posts' face, true
/// where the fence is `kept`, sagging or snapped and hanging where not; laid
/// only where `built`.
fn rail(
    mason: &mut Mason,
    (a, b): ((Point, f64), (Point, f64)),
    height: f64,
    (kept, draws): (bool, &mut Dice),
    built: bool,
) -> Option<()> {
    let span = (b.0 - a.0).length();
    if span < 0.3 {
        return Some(());
    }
    let way = (b.0 - a.0) * (1.0 / span);
    let rise = mathf::atan2(b.1 - a.1, span);
    let face = way.left() * 0.09;
    let (snapped, sag) = if kept {
        (false, 0.0)
    } else {
        (draws.chance(0.12), draws.range(-0.06, 0.02))
    };
    let length = if snapped {
        draws.range(0.45, 0.7) * span
    } else {
        span + 0.12
    };
    // A snapped rail hangs from its post, its free end down on the ground at
    // most.
    let resting =
        mathf::asin((((b.1 - a.1) * length / span + 0.03 - height) / length).clamp(-1.0, 1.0))
            - rise;
    let droop = if snapped {
        (-draws.range(0.3, 0.9)).max(resting)
    } else {
        sag
    };
    let tearing = snapped.then(|| draws.wide());
    if !built {
        return Some(());
    }
    let start = Vec3::new(a.0.x + face.x, a.1 + height, a.0.y + face.y);
    let frame = tipped(along(way), rise + droop);
    let middle = start + frame.x * (0.5 * length);
    mason.unit(
        (
            Pose::new(middle, frame),
            Vec3::new(0.5 * length, RAIL.0, RAIL.1),
        ),
        Form::Block { fan: 0 },
        Dressing::Timber,
    )?;
    tearing.map_or(Some(()), |seed| {
        torn_end(mason, (start + frame.x * length, frame), seed)
    })
}

/// How far a rail's section reaches up and across from its middle.
const RAIL: (f64, f64) = (0.045, 0.019);

/// The break a rail running along `frame`'s `x` snapped at `end`, torn
/// across its grain, drawn from `seed`: it gave under its load, its fibres
/// pulling out of its underside; set a millimetre back into the rail and a
/// little broader than it, so the rail's sawn end never shows through.
fn torn_end(mason: &mut Mason, (end, frame): (Vec3, Frame), seed: u64) -> Option<()> {
    let mut dice = NonCryptoRng::seed_from_u64(seed);
    let running = running(frame.x);
    let (deep, thick) = (RAIL.0 + 5e-4, RAIL.1 + 5e-4);
    let outline = |angle: f64| {
        let way = running.x * mathf::cos(angle) + running.z * mathf::sin(angle);
        (deep / way.dot(frame.y).abs().max(1e-9)).min(thick / way.dot(frame.z).abs().max(1e-9))
    };
    let under = -frame.y;
    let wood = mason.stone();
    let brk = Break {
        centre: end - frame.x * 0.001,
        frame: running,
        outline: &outline,
        tension: mathf::atan2(under.dot(running.z), under.dot(running.x))
            + 0.8 * (dice.next_f64() - 0.5),
        age: 0.25 + 0.25 * dice.next_f64(),
        barked: false,
    };
    mason.torn(&tear(
        &brk,
        Grain {
            wood,
            rot: wood,
            edge: wood,
        },
        &mut dice,
    )?)
}

/// The field gate hung in `gap` of `boundary`: a hanging post and a shutting
/// post either side, and between them five bars braced on two stiles, shut
/// across the gap, swung open into the field, or dropped off a hinge, which
/// one let go has, and is laid by the second of `masons`. How many pieces it
/// was built of.
fn gate(
    stage: &mut Stage,
    laid: &Laid<'_>,
    (boundary, gap): (&Boundary, &Gap),
    masons: (&mut Mason, &mut Mason),
) -> Option<usize> {
    let mut draws = Dice::keyed(gap.key, 0);
    let half = 0.5 * gap.width;
    let mut walk = Walk::new(laid.line);
    let (Some((hinge, way)), Some((latch, _))) =
        (walk.at(gap.along - half), walk.at(gap.along + half))
    else {
        return Some(0);
    };
    let hung = draws.unit();
    let mason = if (0.35..0.5).contains(&hung) {
        masons.1
    } else {
        masons.0
    };
    let (hinge_ground, latch_ground) = (
        laid.ground(&stage.fields, hinge),
        laid.ground(&stage.fields, latch),
    );
    post(
        mason,
        (hinge, hinge_ground),
        (1.45, 0.1),
        (0.01, &mut draws),
        true,
    )?;
    post(
        mason,
        (latch, latch_ground),
        (1.3, 0.08),
        (0.02, &mut draws),
        true,
    )?;
    // It swings into whichever side is a field, and is hung clear of the
    // ground at its hinge; a turn about the vertical swings it rightward.
    let into = if matches!(boundary.left, Side::Field(_)) {
        -1.0
    } else {
        1.0
    };
    let width = (latch - hinge).length() - 0.2;
    let mut frame = along(way);
    if hung < 0.35 {
        frame = frame.rotated_by(Frame::about(Vec3::UP, into * draws.range(1.2, 1.9)));
    } else if hung < 0.5 {
        // Off its top hinge, its far end down on the ground.
        let drop = mathf::atan2(0.85 - 0.3 * draws.unit(), width);
        frame = tipped(frame, -drop);
    }
    let foot = Vec3::new(hinge.x, hinge_ground + 0.1, hinge.y) + frame.x * 0.11;
    let piece = |mason: &mut Mason,
                 (from, to): ((f64, f64), (f64, f64)),
                 (deep, thick): (f64, f64)| {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let length = mathf::hypot(dx, dy);
        let tilt = mathf::atan2(dy, dx);
        let middle =
            foot + frame.x * f64::midpoint(from.0, to.0) + frame.y * f64::midpoint(from.1, to.1);
        mason.unit(
            (
                Pose::new(middle, tipped(frame, tilt)),
                Vec3::new(0.5 * length, deep, thick),
            ),
            Form::Block { fan: 0 },
            Dressing::Timber,
        )
    };
    piece(mason, ((0.0, 0.0), (0.0, 1.1)), (0.05, 0.04))?;
    piece(mason, ((width, 0.05), (width, 1.06)), (0.035, 0.035))?;
    for (index, &height) in [0.12, 0.34, 0.56, 0.78, 1.04].iter().enumerate() {
        let deep = if index == 4 { 0.055 } else { 0.035 };
        piece(mason, ((0.0, height), (width, height)), (deep, 0.02))?;
    }
    piece(mason, ((0.08, 0.14), (0.62 * width, 1.0)), (0.03, 0.018))?;
    claim(stage, &[hinge, latch], 0.4)?;
    Some(10)
}

/// The stile a path crosses `boundary` by in `gap`: a step on two short
/// posts beside a hedge or fence, or a squeeze between two upright stones
/// in a wall. How many pieces it was built of.
fn stile(
    stage: &mut Stage,
    laid: &Laid<'_>,
    (boundary, gap): (&Boundary, &Gap),
    (timber, stone): (&mut Mason, &mut Mason),
) -> Option<usize> {
    let mut draws = Dice::keyed(gap.key, 1);
    let Some((at, way)) = plane::at(laid.line, gap.along) else {
        return Some(0);
    };
    let ground = laid.ground(&stage.fields, at);
    if boundary.kind == Bound::Wall {
        // Two slabs set on end as they were found, broken angular.
        for side in [-1.0, 1.0] {
            let place = at + way * (side * 0.28);
            let tall = draws.range(1.0, 1.25);
            let frame = leant(along(way), draws.range(-0.05, 0.05));
            let form = Form::Rock {
                round: draws.count(10, 40).try_into().unwrap_or(25),
                facets: draws.count(4, 6).try_into().unwrap_or(5),
            };
            stone.unit(
                (
                    Pose::new(
                        Vec3::new(place.x, ground + 0.5 * tall - 0.2, place.y),
                        frame,
                    ),
                    Vec3::new(0.11, 0.5 * tall + 0.2, 0.3),
                ),
                form,
                Dressing::Field,
            )?;
        }
        return Some(2);
    }
    for side in [-1.0, 1.0] {
        let place = at + way * (side * 0.45);
        post(
            timber,
            (place, laid.ground(&stage.fields, place)),
            (0.95, 0.055),
            (0.03, &mut draws),
            true,
        )?;
    }
    let step = Vec3::new(at.x, ground + 0.38, at.y);
    timber.unit(
        (Pose::new(step, along(way)), Vec3::new(0.55, 0.025, 0.12)),
        Form::Block { fan: 0 },
        Dressing::Timber,
    )?;
    claim(stage, &[at - way * 0.6, at + way * 0.6], 0.5)?;
    Some(3)
}

/// What shows of every boundary of `boundaries` of `kind` — a hedge's
/// shaded foot, a wall's grey — on `land`'s ground where it runs, but across
/// its gaps, to be indexed; `None` when the heap will not hold it.
fn painted(boundaries: &[Boundary], land: &Land, kind: Bound) -> Option<Indexing> {
    let width = match kind {
        Bound::Hedge => 2.2,
        Bound::Wall => 0.75,
        Bound::Fence | Bound::Ditch | Bound::Open => 0.0,
    };
    let mut courses: Vec<Vec<Mark>> = Vec::new();
    for boundary in boundaries.iter().filter(|boundary| boundary.kind == kind) {
        let length = plane::length(&boundary.line);
        for span in standing(&boundary.gaps, (0.0, length), 0.5).ok()? {
            let course = traced(&boundary.line, span, width)?;
            courses.try_reserve(1).ok()?;
            courses.push(course);
        }
    }
    let ((cx, cz), reach) = (land.grids.centre, land.grids.reach);
    let marked = Reach {
        per_width: 1.0,
        beyond: 2.0,
    };
    Indexing::new(courses, ((cx - reach, cz - reach), 2.0 * reach), marked)
}

#[cfg(test)]
#[path = "fields_tests.rs"]
mod tests;

//! A region's countryside laid out a bounded unit at a time: its villages,
//! holdings and farmsteads, the ways between them routed rank by rank, the
//! villages' plots, each holding's fields, their boundaries and gateways, and
//! what each field is used for.
//!
//! Each feature is worked out over as much of the land about it as its making
//! reads — a lane follows the roads its lattice reaches, a field's boundary
//! reads the holding beyond it — so whichever region asks, a feature comes out
//! the same: a region laid out in pieces lays every feature exactly as one
//! laid out whole, and the layout reports every feature reaching its region.

use alloc::vec::Vec;

use tairix_parallel::{for_each, JobRunner};
use tairix_util::mathf;

use crate::boundary::{self, Boundary, Kind, Side, Style, Survey};
use crate::farm::{self, Farmstead};
use crate::field::{self, Cell, Field, Parcels, Surround};
use crate::ground::{self, Ground, Lie};
use crate::holding::{HoldingId, Lattice};
use crate::key::{Key, Stage};
use crate::network::{self, Filed, Joins, Node, Placed, Rank, WayId};
use crate::plane::{self, Buckets, Convex, Point, Rect};
use crate::route::{Line, Routing, Square};
use crate::site::{self, Settled, Settlement, Villages};
use crate::usage::{self, Mix, Usage};
use crate::village::{self, Village, REACH};
use crate::Error;

/// A highway its consumer brings, routed as the consumer routes its own.
#[derive(Clone, Debug, PartialEq)]
pub struct Highway {
    /// Its number, as its consumer knows it.
    pub number: u32,
    /// Its line.
    pub line: Line,
}

/// What a region's countryside is laid out by.
#[derive(Clone, Debug, PartialEq)]
pub struct Countryside {
    /// The key every draw is keyed from.
    pub key: Key,
    /// How far apart its holdings' middles lie, in metres.
    pub spacing: f64,
    /// Its villages.
    pub villages: Villages,
    /// Its custom in boundaries.
    pub style: Style,
    /// Its custom in what its fields are used for.
    pub mix: Mix,
    /// The unit way the noon sun stands toward.
    pub warm: Point,
    /// The highways its consumer brings.
    pub highways: Vec<Highway>,
}

/// The narrowest and widest a holding's spacing may be, in metres.
const SPACING: (f64, f64) = (150.0, 4000.0);

/// A way laid out.
#[derive(Clone, Debug, PartialEq)]
pub struct Way {
    /// Which it is.
    pub id: WayId,
    /// Its line.
    pub line: Line,
    /// The word everything drawn of it is keyed from.
    pub key: u64,
    /// The rectangle its line lies in, its corridor's reach about it.
    pub(crate) bounds: Rect,
}

impl Way {
    /// The way `id` along `line`, keyed under `key`; `None` for a line of no
    /// station.
    fn of(key: Key, id: WayId, line: Line) -> Option<Self> {
        let reach = 0.5 * line.stations.iter().map(|station| station.width).fold(0.0, f64::max)
            + id.rank.laying().verge;
        let bounds = line.bounds(reach)?;
        let mut hasher = key.hasher(Stage::Way);
        id.write(&mut hasher);
        Some(Self {
            id,
            line,
            key: core::hash::Hasher::finish(&hasher),
            bounds,
        })
    }
}

/// A field, and what it is used for.
#[derive(Clone, Debug, PartialEq)]
pub struct Parcel {
    /// The field.
    pub field: Field,
    /// What it is used for.
    pub usage: Usage,
}

/// How far apart a holding's corridor buckets lie: wider than any way's
/// corridor reaches, so a bucket lists every corridor that may reach a point
/// in it.
const BUCKET: f64 = 16.0;

/// A holding, as a layout keeps it.
#[derive(Clone, Debug)]
struct Holding {
    id: HoldingId,
    vertex: Point,
    outline: Convex,
    bounds: Rect,
    /// Its farmstead, by its index among the layout's.
    farm: Option<u32>,
    /// The village that gathers its farm, where one does.
    home: Option<Point>,
    custom: Kind,
    parcels: Option<Parcels>,
    /// The plots reaching it.
    plots: Vec<Convex>,
    /// Each way's stretch whose corridor reaches it, the way by its index
    /// among the layout's ways, and the stretch by its first station.
    stretches: Vec<(u32, u32)>,
    /// Those stretches, filed by where their corridors reach.
    corridors: Buckets,
}

impl Holding {
    /// Whether anybody farms it: its own farmstead, or a village's farms.
    const fn farmed(&self) -> bool {
        self.farm.is_some() || self.home.is_some()
    }
}

/// What the land is, as a layout's holdings and ways read it.
struct Land<'a> {
    lattice: &'a Lattice,
    holdings: &'a [Holding],
    ways: &'a [Way],
    ground: &'a dyn Ground,
}

impl Land<'_> {
    fn holding(&self, id: HoldingId) -> Option<&Holding> {
        self.holdings
            .binary_search_by(|holding| holding.id.cmp(&id))
            .ok()
            .and_then(|index| self.holdings.get(index))
    }

    /// The holding `at` lies in, by the outlines held where they are.
    fn holding_at(&self, at: Point) -> HoldingId {
        self.lattice.locate(at, &|id| match self.holding(id) {
            Some(holding) => (holding.outline.contains(at), holding.vertex),
            None => (self.lattice.outline(id).contains(at), self.lattice.vertex(id)),
        })
    }

    /// The way whose corridor holds `at`, the nearest its line where more
    /// than one does.
    fn corridor(&self, holding: &Holding, at: Point) -> Option<WayId> {
        let mut nearest: Option<(f64, WayId)> = None;
        for item in holding.corridors.at(at) {
            let Some(&(way, segment)) = holding.stretches.get(item as usize) else {
                continue;
            };
            let Some(way) = self.ways.get(way as usize) else {
                continue;
            };
            let segment = segment as usize;
            let (Some(a), Some(b)) = (way.line.stations.get(segment), way.line.stations.get(segment + 1)) else {
                continue;
            };
            let reach = 0.5 * a.width.max(b.width) + way.id.rank.laying().verge;
            let squared = plane::onto_segment(at, a.at, b.at).1;
            if squared < reach * reach
                && nearest.is_none_or(|(least, id)| (squared, way.id) < (least, id))
            {
                nearest = Some((squared, way.id));
            }
        }
        nearest.map(|(_, id)| id)
    }

    /// What lies at `at`, when its cell's middle lay over what bars a field.
    fn resolve(&self, holding: &Holding, parcels: &Parcels, (column, row): (usize, usize), at: Point) -> Side {
        if let Some(way) = self.corridor(holding, at) {
            return Side::Way(way);
        }
        if ground::wet(self.ground, at) {
            return Side::Water;
        }
        if holding.plots.iter().any(|plot| plot.contains(at)) {
            return Side::Plot;
        }
        match parcels.cell(column, row) {
            Some(Cell::Land(block)) => return field_side(holding.id, parcels, block, at),
            Some(Cell::Waste) => return Side::Waste,
            _ => {}
        }
        // Clear of all that barred its cell's middle, `at` lies on the land
        // of the nearest cell beside it.
        let mut nearest: Option<(f64, Cell)> = None;
        for (dc, dr) in AROUND_CELL {
            let (Some(c), Some(r)) = (column.checked_add_signed(dc), row.checked_add_signed(dr)) else {
                continue;
            };
            let Some(cell) = parcels.cell(c, r).filter(|cell| matches!(cell, Cell::Land(_) | Cell::Waste)) else {
                continue;
            };
            let apart = (parcels.middle(c, r) - at).length();
            if nearest.is_none_or(|(least, _)| apart < least) {
                nearest = Some((apart, cell));
            }
        }
        match nearest {
            Some((_, Cell::Land(block))) => field_side(holding.id, parcels, block, at),
            _ => Side::Waste,
        }
    }
}

/// The eight cells about a cell, by column and row.
const AROUND_CELL: [(isize, isize); 8] = [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)];

/// The field of `holding`'s block `block` at `at`.
fn field_side(holding: HoldingId, parcels: &Parcels, block: u16, at: Point) -> Side {
    parcels.field_in(block, at).map_or(Side::Waste, |index| {
        Side::Field(crate::field::FieldId { holding, index })
    })
}

impl Survey for Land<'_> {
    fn side(&self, at: Point) -> Side {
        let id = self.holding_at(at);
        let Some(holding) = self.holding(id) else {
            return Side::Out;
        };
        let Some(parcels) = holding.parcels.as_ref() else {
            // Land nobody farms lies open beyond the fields' last boundary.
            return if holding.farmed() { Side::Out } else { Side::Waste };
        };
        let Some((column, row)) = parcels.position(at) else {
            return Side::Out;
        };
        match parcels.cell(column, row) {
            // A cell clear of every corridor, but for one beside water or a
            // plot, which may take a part of it.
            Some(Cell::Land(block)) if !borders(parcels, (column, row)) => field_side(id, parcels, block, at),
            Some(Cell::Waste) if !borders(parcels, (column, row)) => Side::Waste,
            _ => self.resolve(holding, parcels, (column, row), at),
        }
    }

    fn lie(&self, at: Point) -> Lie {
        self.ground.lie(at)
    }
}

/// Whether the cell at `column` and `row` borders water or a plot.
fn borders(parcels: &Parcels, (column, row): (usize, usize)) -> bool {
    AROUND_CELL.iter().any(|&(dc, dr)| {
        let (Some(c), Some(r)) = (column.checked_add_signed(dc), row.checked_add_signed(dr)) else {
            return false;
        };
        matches!(parcels.cell(c, r), Some(Cell::Water | Cell::Plot))
    })
}

/// A region's countryside, laid out: every feature reaching the region.
#[derive(Clone, Debug)]
pub struct Layout {
    region: Rect,
    lattice: Lattice,
    /// The ways reaching the region, and those whose corridors reach its
    /// holdings, in their order.
    ways: Vec<Way>,
    reaching: Vec<u32>,
    holdings: Vec<Holding>,
    parcels: Vec<Parcel>,
    boundaries: Vec<Boundary>,
    settlements: Vec<Settlement>,
    farmsteads: Vec<Farmstead>,
    villages: Vec<Village>,
}

impl Layout {
    /// The region it reports.
    #[must_use]
    pub const fn region(&self) -> Rect {
        self.region
    }

    /// The ways reaching the region, in their order.
    pub fn ways(&self) -> impl Iterator<Item = &Way> + '_ {
        self.reaching.iter().filter_map(|&index| self.ways.get(index as usize))
    }

    /// The fields reaching the region, each with its use, in their order.
    #[must_use]
    pub fn parcels(&self) -> &[Parcel] {
        &self.parcels
    }

    /// The boundaries reaching the region, in their order.
    #[must_use]
    pub fn boundaries(&self) -> &[Boundary] {
        &self.boundaries
    }

    /// The villages and farmsteads whose plots reach the region.
    #[must_use]
    pub fn settlements(&self) -> &[Settlement] {
        &self.settlements
    }

    /// The farmsteads reaching the region, laid out.
    #[must_use]
    pub fn farmsteads(&self) -> &[Farmstead] {
        &self.farmsteads
    }

    /// The villages whose plots reach the region, laid out.
    #[must_use]
    pub fn villages(&self) -> &[Village] {
        &self.villages
    }

    /// What lies at `at`, within the region, over `ground`: the field there,
    /// or the way, water, plot or waste; `Out` beyond the region's holdings.
    #[must_use]
    pub fn side_at(&self, at: Point, ground: &dyn Ground) -> Side {
        Land {
            lattice: &self.lattice,
            holdings: &self.holdings,
            ways: &self.ways,
            ground,
        }
        .side(at)
    }

    /// The field at `at` over `ground`, and its use; `None` where no field is.
    #[must_use]
    pub fn parcel_at(&self, at: Point, ground: &dyn Ground) -> Option<&Parcel> {
        let Side::Field(id) = self.side_at(at, ground) else {
            return None;
        };
        self.parcels
            .binary_search_by(|parcel| parcel.field.id.cmp(&id))
            .ok()
            .and_then(|index| self.parcels.get(index))
    }
}

/// How far the ways of each rank run at the longest, and the regions a
/// layout lays out each part of over.
#[derive(Copy, Clone, Debug)]
struct Regions {
    longest: [f64; 5],
    /// The holdings given fields: the region's, and their neighbours'.
    hold: Rect,
    /// Where the bounded ways must be known, for those holdings' fields.
    bounded: Rect,
    /// Where each rank's ways must be known, by rank.
    need: [Rect; 5],
    /// The villages whose plots are laid: wherever a track's or path's
    /// lattice or a holding's fields may meet them.
    plotted: Rect,
    /// The holdings given farmsteads and yards.
    farms: Rect,
    /// The holdings looked at for farmsteads, those given yards and those
    /// about them.
    holdings: Rect,
    /// The villages looked at.
    villages: Rect,
}

/// How far a village's plots reach from its middle.
const VILLAGE_REACH: f64 = REACH.1 + 60.0;

impl Regions {
    fn of(countryside: &Countryside, region: Rect) -> Self {
        let spacing = countryside.spacing;
        let mut longest = [0.0; 5];
        longest[Rank::Road as usize] = 2.0 * countryside.villages.spacing;
        longest[Rank::Lane as usize] = 2.2 * spacing;
        longest[Rank::Track as usize] = 1.6 * spacing;
        longest[Rank::Path as usize] = 1.6 * spacing;
        // A lattice of `rank` that reaches a region runs no further past it.
        let past = |rank: Rank| 1.6 * longest[rank as usize] + 8.0 * rank.laying().step;
        let hold = region.grown(1.6 * spacing);
        let bounded = hold.grown(0.8 * spacing);
        let path = hold;
        let track = bounded;
        // A village's plots are laid wherever a track or path keeps clear of
        // them, and a lane is known wherever those plots front a street, a
        // track or path may follow it, or a holding's tracks may begin on it.
        let plotted = union(bounded.grown(past(Rank::Track)), path.grown(past(Rank::Path))).grown(VILLAGE_REACH);
        let lane = union(
            plotted.grown(VILLAGE_REACH),
            bounded.grown(past(Rank::Track) + 0.9 * spacing),
        );
        let road = lane.grown(past(Rank::Lane) + 0.5 * spacing);
        // A way's ends, and the places that decide whether it is joined at all.
        let joined = |within: Rect, rank: Rank| within.grown(past(rank) + longest[rank as usize]);
        let farms = union(
            union(road.grown(past(Rank::Road)), joined(lane, Rank::Lane)),
            union(joined(path, Rank::Path), track.grown(past(Rank::Track))),
        );
        let holdings = farms.grown(longest[Rank::Lane as usize]);
        let villages = union(
            joined(road, Rank::Road),
            holdings.grown(countryside.villages.gathers * countryside.villages.spacing + spacing),
        );
        let mut need = [region; 5];
        need[Rank::Highway as usize] = road;
        need[Rank::Road as usize] = road;
        need[Rank::Lane as usize] = lane;
        need[Rank::Track as usize] = track;
        need[Rank::Path as usize] = path;
        Self {
            longest,
            hold,
            bounded,
            need,
            plotted,
            farms,
            holdings,
            villages,
        }
    }
}

/// The least rectangle holding both `a` and `b`.
fn union(a: Rect, b: Rect) -> Rect {
    a.including(b.low).including(b.high)
}

/// What a job run across a runner came to, once it has run.
type Outcome<T> = Option<Result<T, Error>>;

/// Gaps to hang, each by the index of its boundary.
type Gaps = Vec<(u32, boundary::Gap)>;

/// Shapes, each with the rectangle it lies in.
type Shapes = [(Rect, Convex)];

/// A way to be routed.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Planned {
    id: WayId,
    /// Where it runs from and to: its first end's place, then its second's.
    ends: (Point, Point),
}

/// A way being routed, and what its last unit came to.
#[derive(Debug)]
struct Active {
    planned: Planned,
    routing: Option<Routing>,
    outcome: Outcome<Option<Line>>,
}

/// Where a layout's laying stands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Phase {
    Settle,
    Plan(Rank),
    Route(Rank),
    Villages,
    Parcels,
    Bounds,
    Gates,
    Done,
}

/// A region's countryside being laid out.
#[derive(Debug)]
pub struct Laying {
    countryside: Countryside,
    region: Rect,
    lattice: Lattice,
    regions: Regions,
    phase: Phase,
    villages: Vec<Settlement>,
    holdings: Vec<Holding>,
    farmsteads: Vec<Farmstead>,
    /// Each farmstead's plot.
    yards: Vec<(Rect, Convex)>,
    /// What every way keeps clear of: each farmstead's buildings and garden.
    barred: Vec<(Rect, Convex)>,
    /// What a track or path keeps clear of besides: the villages' plots.
    plotted: Vec<(Rect, Convex)>,
    ways: Vec<Way>,
    queue: Vec<Planned>,
    /// How many ways of the rank being routed were planned.
    planned: usize,
    active: Vec<Active>,
    laid_villages: Vec<Village>,
    boundaries: Vec<Boundary>,
}

/// Each phase's share of a laying's work, in the order they run, and the
/// share before it: measured over a farmed land, the routes dearest.
const SHARES: [(Phase, f64); 12] = [
    (Phase::Settle, 0.04),
    (Phase::Plan(Rank::Road), 0.0),
    (Phase::Route(Rank::Road), 0.2),
    (Phase::Plan(Rank::Lane), 0.0),
    (Phase::Route(Rank::Lane), 0.36),
    (Phase::Villages, 0.02),
    (Phase::Plan(Rank::Track), 0.0),
    (Phase::Route(Rank::Track), 0.1),
    (Phase::Plan(Rank::Path), 0.0),
    (Phase::Route(Rank::Path), 0.08),
    (Phase::Parcels, 0.12),
    (Phase::Bounds, 0.08),
];

impl Laying {
    /// The laying of `countryside` over `region`; `Err` where its holdings'
    /// spacing or its villages' are outside what a layout lays out.
    pub fn new(countryside: Countryside, region: Rect) -> Result<Self, Error> {
        let spacing = countryside.spacing;
        let villages = countryside.villages;
        let sane = (SPACING.0..=SPACING.1).contains(&spacing)
            && villages.spacing >= 2.0 * spacing
            && villages.spacing <= 64.0 * spacing
            && (0.0..=1.0).contains(&villages.exclusion)
            && (0.0..=1.0).contains(&villages.gathers)
            && region.low.x <= region.high.x
            && region.low.y <= region.high.y;
        if !sane {
            return Err(Error::Shape);
        }
        Ok(Self {
            lattice: Lattice::new(spacing, countryside.key),
            regions: Regions::of(&countryside, region),
            countryside,
            region,
            phase: Phase::Settle,
            villages: Vec::new(),
            holdings: Vec::new(),
            farmsteads: Vec::new(),
            yards: Vec::new(),
            barred: Vec::new(),
            plotted: Vec::new(),
            ways: Vec::new(),
            queue: Vec::new(),
            planned: 0,
            active: Vec::new(),
            laid_villages: Vec::new(),
            boundaries: Vec::new(),
        })
    }

    /// How far the laying has come, as a share of its work.
    #[must_use]
    pub fn done(&self) -> f64 {
        if self.phase == Phase::Done {
            return 1.0;
        }
        let mut before = 0.0;
        for (phase, share) in SHARES {
            if phase == self.phase {
                let within = match phase {
                    Phase::Route(_) if self.planned > 0 => {
                        let left = self.queue.len() + self.active.len();
                        1.0 - real(left.min(self.planned)) / real(self.planned)
                    }
                    _ => 0.0,
                };
                return before + share * within;
            }
            before += share;
        }
        before
    }

    /// The next unit of the laying across `runner` over `ground`: the layout
    /// once it is laid, `None` while it is not; `Err` where the heap refused
    /// it, or where it is asked for a unit after its layout.
    pub fn step(&mut self, ground: &dyn Ground, runner: &dyn JobRunner) -> Result<Option<Layout>, Error> {
        match self.phase {
            Phase::Settle => {
                self.settle(ground, runner)?;
                self.phase = Phase::Plan(Rank::Road);
            }
            Phase::Plan(rank) => {
                self.plan(rank, ground)?;
                self.phase = Phase::Route(rank);
            }
            Phase::Route(rank) => {
                if self.route(rank, ground, runner)? {
                    self.phase = match rank {
                        Rank::Highway | Rank::Road => Phase::Plan(Rank::Lane),
                        Rank::Lane => Phase::Villages,
                        Rank::Track => Phase::Plan(Rank::Path),
                        Rank::Path => Phase::Parcels,
                    };
                }
            }
            Phase::Villages => {
                self.lay_villages(ground, runner)?;
                self.phase = Phase::Plan(Rank::Track);
            }
            Phase::Parcels => {
                self.parcel(ground, runner)?;
                self.phase = Phase::Bounds;
            }
            Phase::Bounds => {
                self.bound(ground, runner)?;
                self.phase = Phase::Gates;
            }
            Phase::Gates => {
                self.gate(runner)?;
                self.phase = Phase::Done;
                return self.laid(runner).map(Some);
            }
            Phase::Done => return Err(Error::Shape),
        }
        Ok(None)
    }

    /// The whole laying at once, across `runner` over `ground`.
    pub fn run(mut self, ground: &dyn Ground, runner: &dyn JobRunner) -> Result<Layout, Error> {
        loop {
            if let Some(layout) = self.step(ground, runner)? {
                return Ok(layout);
            }
        }
    }

    const fn key(&self) -> Key {
        self.countryside.key
    }

    fn holding(&self, id: HoldingId) -> Option<&Holding> {
        self.holdings
            .binary_search_by(|holding| holding.id.cmp(&id))
            .ok()
            .and_then(|index| self.holdings.get(index))
    }

    /// The villages, the holdings with their farmsteads, the farmsteads'
    /// yards, and the highways.
    fn settle(&mut self, ground: &dyn Ground, runner: &dyn JobRunner) -> Result<(), Error> {
        let key = self.key();
        let tier = self.countryside.villages;
        self.villages = site::villages(key, &tier, ground, self.regions.villages).ok_or(Error::OutOfMemory)?;
        self.villages.sort_unstable_by_key(|village| village.settled);
        let ids = self
            .lattice
            .holdings_over(self.regions.holdings)
            .ok_or(Error::OutOfMemory)?;
        let mut holdings: Vec<(Holding, Option<Settlement>)> = Vec::new();
        holdings.try_reserve_exact(ids.len()).map_err(|_| Error::OutOfMemory)?;
        for id in ids {
            let outline = self.lattice.outline(id);
            let bounds = outline.bounds().ok_or(Error::Shape)?;
            holdings.push((
                Holding {
                    id,
                    vertex: self.lattice.vertex(id),
                    outline,
                    bounds,
                    farm: None,
                    home: None,
                    custom: Kind::Hedge,
                    parcels: None,
                    plots: Vec::new(),
                    stretches: Vec::new(),
                    corridors: Buckets::default(),
                },
                None,
            ));
        }
        let gathers = tier.gathers * tier.spacing;
        let (lattice, villages, style) = (&self.lattice, &self.villages[..], self.countryside.style);
        for_each(runner, &mut holdings, &|(holding, farm)| {
            let mut draws = key.draws(Stage::Custom, holding.id.place());
            holding.custom = style.custom(ground.lie(holding.vertex), &mut draws);
            holding.home = site::gatherer(villages, holding.vertex, gathers).map(|village| village.at);
            *farm = site::farmstead(key, lattice, ground, (holding.id, villages, gathers));
        });
        holdings.sort_unstable_by_key(|(holding, _)| holding.id);
        self.lay_yards(&holdings, ground, runner)?;
        self.holdings.try_reserve_exact(holdings.len()).map_err(|_| Error::OutOfMemory)?;
        self.holdings.extend(holdings.into_iter().map(|(holding, _)| holding));
        for (index, farm) in self.farmsteads.iter().enumerate() {
            let Settled::Farmstead(id) = farm.settled else {
                continue;
            };
            if let Ok(at) = self.holdings.binary_search_by(|holding| holding.id.cmp(&id)) {
                self.holdings[at].farm = u32::try_from(index).ok();
            }
            let plot = farm.plot.bounds().ok_or(Error::Shape)?;
            self.yards.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
            self.yards.push((plot, farm.plot.clone()));
            for shape in farm
                .buildings
                .iter()
                .map(farm::Footprint::outline)
                .chain(core::iter::once(farm.garden.clone()))
            {
                let bounds = shape.bounds().ok_or(Error::Shape)?;
                self.barred.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                self.barred.push((bounds, shape));
            }
        }
        self.ways
            .try_reserve(self.countryside.highways.len())
            .map_err(|_| Error::OutOfMemory)?;
        for highway in &self.countryside.highways {
            let id = WayId {
                rank: Rank::Highway,
                joins: Joins::Highway(highway.number),
            };
            if let Some(way) = Way::of(key, id, highway.line.clone()) {
                self.ways.push(way);
            }
        }
        self.ways.sort_unstable_by_key(|way| way.id);
        Ok(())
    }

    /// The yard of every farmstead of `holdings` where ways may reach it,
    /// each facing the nearest place it may be joined to.
    fn lay_yards(
        &mut self,
        holdings: &[(Holding, Option<Settlement>)],
        ground: &dyn Ground,
        runner: &dyn JobRunner,
    ) -> Result<(), Error> {
        let key = self.key();
        let mut places: Vec<Point> = Vec::new();
        places
            .try_reserve_exact(holdings.len() + self.villages.len())
            .map_err(|_| Error::OutOfMemory)?;
        places.extend(holdings.iter().filter_map(|(_, farm)| farm.map(|farm| farm.at)));
        places.extend(self.villages.iter().map(|village| village.at));
        let reach = self.regions.longest[Rank::Lane as usize];
        let filed = Filed::new(places.iter().copied(), reach).ok_or(Error::OutOfMemory)?;
        let mut yards: Vec<(Settlement, Point, Outcome<Farmstead>)> = Vec::new();
        for (holding, farm) in holdings {
            let Some(farm) = farm else {
                continue;
            };
            if !holding.bounds.overlaps(self.regions.farms) {
                continue;
            }
            let toward = filed
                .near(farm.at, reach)
                .filter_map(|index| places.get(index))
                .map(|&at| at - farm.at)
                .filter(|apart| apart.length() > 1.0 && apart.length() <= reach)
                .min_by(|a, b| {
                    a.length()
                        .total_cmp(&b.length())
                        .then(a.x.total_cmp(&b.x))
                        .then(a.y.total_cmp(&b.y))
                })
                .map_or_else(
                    || {
                        let mut draws = key.draws(Stage::Yard, farm.settled.place());
                        Point::toward(draws.range(0.0, core::f64::consts::TAU))
                    },
                    Point::normalized,
                );
            yards.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
            yards.push((*farm, toward, None));
        }
        for_each(runner, &mut yards, &|(farm, toward, laid)| {
            *laid = Some(farm::lay_out(key, farm, *toward, ground));
        });
        self.farmsteads.try_reserve_exact(yards.len()).map_err(|_| Error::OutOfMemory)?;
        for (_, _, laid) in yards {
            self.farmsteads.push(laid.ok_or(Error::Shape)??);
        }
        self.farmsteads.sort_unstable_by_key(|farm| farm.settled);
        Ok(())
    }

    /// The farmstead of holding `id`, where it has one.
    fn farm_of(&self, id: HoldingId) -> Option<&Farmstead> {
        let index = self.holding(id)?.farm?;
        self.farmsteads.get(index as usize)
    }

    /// The village `settled`, where it stands.
    fn village(&self, settled: Settled) -> Option<&Settlement> {
        self.villages
            .binary_search_by(|village| village.settled.cmp(&settled))
            .ok()
            .and_then(|index| self.villages.get(index))
    }

    /// Where a way ending at the settlement `placed` ends: a village's
    /// middle, a farmstead's yard gate.
    fn end_of(&self, placed: Placed) -> Option<Point> {
        match placed {
            Placed::Settled(settled @ Settled::Village(..)) => self.village(settled).map(|village| village.at),
            Placed::Settled(Settled::Farmstead(id)) => self.farm_of(id).map(|farm| farm.gate),
            Placed::Gateway(..) | Placed::OnHighway(..) | Placed::OnRoad(..) => None,
        }
    }

    /// The ways of `rank` to route: those whose lattices reach where the
    /// rank's ways must be known, not yet laid, in their order.
    fn plan(&mut self, rank: Rank, ground: &dyn Ground) -> Result<(), Error> {
        let mut planned = match rank {
            Rank::Highway | Rank::Road => self.plan_roads()?,
            Rank::Lane => self.plan_lanes()?,
            Rank::Track => self.plan_tracks(ground)?,
            Rank::Path => self.plan_paths()?,
        };
        let need = self.regions.need[rank as usize];
        planned.retain(|way| {
            (way.ends.1 - way.ends.0).length() >= 2.0 * rank.laying().step
                && Square::of(rank, way.ends).is_ok_and(|square| square.rect().overlaps(need))
                && self.ways.binary_search_by(|laid| laid.id.cmp(&way.id)).is_err()
        });
        // Popped from the back, so routed in their order.
        planned.sort_unstable_by_key(|way| core::cmp::Reverse(way.id));
        planned.dedup_by(|a, b| a.id == b.id);
        self.planned = planned.len();
        self.queue = planned;
        Ok(())
    }

    /// The roads: between the villages a relative-neighbourhood graph joins,
    /// and from each village near a highway to it.
    fn plan_roads(&self) -> Result<Vec<Planned>, Error> {
        let mut nodes: Vec<Node> = Vec::new();
        nodes.try_reserve_exact(self.villages.len()).map_err(|_| Error::OutOfMemory)?;
        nodes.extend(self.villages.iter().map(Node::of));
        let longest = self.regions.longest[Rank::Road as usize];
        let edges = network::neighbourhood(&nodes, longest, &|_, _| true).ok_or(Error::OutOfMemory)?;
        let mut planned = Vec::new();
        planned.try_reserve(edges.len() + nodes.len()).map_err(|_| Error::OutOfMemory)?;
        for (a, b) in edges {
            planned.push(between(Rank::Road, (nodes[a].placed, nodes[a].at), (nodes[b].placed, nodes[b].at)));
        }
        let spur = 0.75 * self.countryside.villages.spacing;
        for village in &self.villages {
            if let Some((placed, at)) = self.nearest_on(&[Rank::Highway], village.at, spur) {
                planned.push(between(Rank::Road, (Placed::Settled(village.settled), village.at), (placed, at)));
            }
        }
        Ok(planned)
    }

    /// The lanes: between the farmsteads, and from them to the villages, as a
    /// relative-neighbourhood graph joins them; and from a farmstead hard by
    /// a road or highway to it.
    fn plan_lanes(&self) -> Result<Vec<Planned>, Error> {
        let nodes = self.settlements()?;
        let longest = self.regions.longest[Rank::Lane as usize];
        let both_villages = |a: &Node, b: &Node| {
            matches!(
                (a.placed, b.placed),
                (Placed::Settled(Settled::Village(..)), Placed::Settled(Settled::Village(..)))
            )
        };
        let edges = network::neighbourhood(&nodes, longest, &|a, b| !both_villages(a, b)).ok_or(Error::OutOfMemory)?;
        let mut planned = Vec::new();
        planned
            .try_reserve(edges.len() + self.farmsteads.len())
            .map_err(|_| Error::OutOfMemory)?;
        for (a, b) in edges {
            let (Some(from), Some(to)) = (self.end_of(nodes[a].placed), self.end_of(nodes[b].placed)) else {
                continue;
            };
            planned.push(between(Rank::Lane, (nodes[a].placed, from), (nodes[b].placed, to)));
        }
        let spur = 0.35 * self.countryside.spacing;
        for farm in &self.farmsteads {
            if let Some((placed, at)) = self.nearest_on(&[Rank::Highway, Rank::Road], farm.gate, spur) {
                planned.push(between(Rank::Lane, (Placed::Settled(farm.settled), farm.gate), (placed, at)));
            }
        }
        Ok(planned)
    }

    /// Every farmstead and village, as the network's nodes.
    fn settlements(&self) -> Result<Vec<Node>, Error> {
        let mut nodes: Vec<Node> = Vec::new();
        nodes
            .try_reserve_exact(self.farmsteads.len() + self.villages.len())
            .map_err(|_| Error::OutOfMemory)?;
        nodes.extend(self.farmsteads.iter().map(|farm| Node {
            placed: Placed::Settled(farm.settled),
            at: farm.at,
        }));
        nodes.extend(self.villages.iter().map(Node::of));
        Ok(nodes)
    }

    /// The tracks: from each holding's farmstead, or from the nearest greater
    /// way to its middle where a village gathers its farm, out to its fields'
    /// gateways.
    fn plan_tracks(&self, ground: &dyn Ground) -> Result<Vec<Planned>, Error> {
        let key = self.key();
        let spacing = self.countryside.spacing;
        let reach = self.regions.need[Rank::Track as usize]
            .grown(1.6 * self.regions.longest[Rank::Track as usize] + 8.0 * Rank::Track.laying().step);
        let mut planned = Vec::new();
        for holding in &self.holdings {
            if !holding.bounds.overlaps(reach) {
                continue;
            }
            let home = match self.farm_of(holding.id) {
                Some(farm) => Some((Placed::Settled(farm.settled), farm.gate)),
                None if holding.home.is_some() => self
                    .nearest_on(&[Rank::Highway, Rank::Road, Rank::Lane], holding.vertex, 0.9 * spacing)
                    .map(|(_, at)| (Placed::Gateway(holding.id, 0), at)),
                None => None,
            };
            let Some((home, from)) = home else {
                continue;
            };
            let mut draws = key.draws(Stage::Gateway, holding.id.place());
            let count = 1 + draws.below(2);
            let any_way = Point::toward(draws.range(0.0, core::f64::consts::TAU));
            let away = Some((holding.vertex - from).normalized())
                .filter(|away| away.length() > 0.0)
                .unwrap_or(any_way);
            for gateway in 1..=count {
                let spread = (real(gateway) - 0.5 * real(count + 1)) * 1.4;
                let found = (0..6).find_map(|_| {
                    let heading = away.heading() + spread + draws.range(-0.5, 0.5);
                    let spot = holding.vertex + Point::toward(heading) * (draws.range(0.3, 0.6) * spacing);
                    let clear = holding.outline.contains(spot)
                        && !ground::wet(ground, spot)
                        && (spot - from).length() > 40.0
                        && !self
                            .yards
                            .iter()
                            .chain(&self.plotted)
                            .any(|(bounds, shape)| bounds.contains(spot) && shape.contains(spot));
                    clear.then_some(spot)
                });
                let Some(spot) = found else {
                    continue;
                };
                let index = u8::try_from(gateway).map_err(|_| Error::Shape)?;
                planned.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                planned.push(between(Rank::Track, (home, from), (Placed::Gateway(holding.id, index), spot)));
            }
        }
        Ok(planned)
    }

    /// The footpaths: the shortcuts between settlements the lanes leave out.
    fn plan_paths(&self) -> Result<Vec<Planned>, Error> {
        let nodes = self.settlements()?;
        let longest = self.regions.longest[Rank::Path as usize];
        let edges = network::gabriel_only(&nodes, longest, &|_, _| true).ok_or(Error::OutOfMemory)?;
        let mut planned = Vec::new();
        planned.try_reserve(edges.len()).map_err(|_| Error::OutOfMemory)?;
        for (a, b) in edges {
            let (Some(from), Some(to)) = (self.end_of(nodes[a].placed), self.end_of(nodes[b].placed)) else {
                continue;
            };
            planned.push(between(Rank::Path, (nodes[a].placed, from), (nodes[b].placed, to)));
        }
        Ok(planned)
    }

    /// The point nearest `at` of the laid ways of `ranks` within `reach` of
    /// it that a way may end on — a highway, a road between two villages, a
    /// lane — and how it is placed there; `None` where none runs so near.
    fn nearest_on(&self, ranks: &[Rank], at: Point, reach: f64) -> Option<(Placed, Point)> {
        let mut nearest: Option<(f64, WayId, Placed, Point)> = None;
        for way in &self.ways {
            if !ranks.contains(&way.id.rank) || !way.bounds.grown(reach).contains(at) {
                continue;
            }
            let Some((near, _)) = way.line.nearest(at) else {
                continue;
            };
            if near.distance > reach || nearest.is_some_and(|(least, id, ..)| (least, id) <= (near.distance, way.id)) {
                continue;
            }
            let centimetres = i64::from(mathf::round_i32((near.along * 100.0).clamp(-2.0e9, 2.0e9)));
            let placed = match way.id.joins {
                Joins::Highway(number) => Placed::OnHighway(number, centimetres),
                Joins::Ends(Placed::Settled(a), Placed::Settled(b)) => Placed::OnRoad(a, b, centimetres),
                Joins::Ends(..) => continue,
            };
            let (Some(a), Some(b)) = (way.line.stations.get(near.segment), way.line.stations.get(near.segment + 1)) else {
                continue;
            };
            let (share, _) = plane::onto_segment(at, a.at, b.at);
            nearest = Some((near.distance, way.id, placed, a.at.lerp(b.at, share)));
        }
        nearest.map(|(_, _, placed, at)| (placed, at))
    }

    /// The next unit of the routes of `rank`, as many at once as `runner` is
    /// wide; whether all of them are routed.
    fn route(&mut self, rank: Rank, ground: &dyn Ground, runner: &dyn JobRunner) -> Result<bool, Error> {
        let width = (2 * runner.width()).clamp(1, 64);
        while self.active.len() < width {
            let Some(planned) = self.queue.pop() else {
                break;
            };
            self.active.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
            self.active.push(Active {
                planned,
                routing: None,
                outcome: None,
            });
        }
        if self.active.is_empty() {
            return Ok(true);
        }
        debug_assert!(self.active.iter().all(|active| active.planned.id.rank == rank));
        let key = self.key();
        let (ways, barred, plotted) = (&self.ways[..], &self.barred[..], &self.plotted[..]);
        for_each(runner, &mut self.active, &|active| {
            active.outcome = Some(advance(active, (key, ground), (ways, barred, plotted)));
        });
        let mut laid = false;
        let mut index = 0;
        while index < self.active.len() {
            match self.active[index].outcome.take() {
                Some(Ok(Some(line))) => {
                    let finished = self.active.swap_remove(index);
                    if let Some(way) = Way::of(key, finished.planned.id, line) {
                        self.ways.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                        self.ways.push(way);
                        laid = true;
                    }
                }
                Some(Ok(None)) | None => index += 1,
                Some(Err(Error::OutOfMemory)) => return Err(Error::OutOfMemory),
                // A way the ground parts, or one with nowhere to run, is not
                // laid at all.
                Some(Err(Error::Shape | Error::Unreachable)) => {
                    self.active.swap_remove(index);
                }
            }
        }
        if laid {
            self.ways.sort_unstable_by_key(|way| way.id);
        }
        Ok(false)
    }

    /// Lay out the villages whose plots could reach the fields' ways.
    fn lay_villages(&mut self, ground: &dyn Ground, runner: &dyn JobRunner) -> Result<(), Error> {
        let key = self.key();
        let reach = self.regions.plotted;
        let mut jobs: Vec<(Settlement, Outcome<Village>)> = Vec::new();
        for village in self.villages.iter().filter(|village| reach.contains(village.at)) {
            jobs.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
            jobs.push((*village, None));
        }
        let (ways, yards) = (&self.ways[..], &self.yards[..]);
        for_each(runner, &mut jobs, &|(village, laid)| {
            let around = Rect::around(village.at, VILLAGE_REACH);
            let streets: Vec<(WayId, &Line)> = ways
                .iter()
                .filter(|way| matches!(way.id.rank, Rank::Highway | Rank::Road | Rank::Lane))
                .filter(|way| way.bounds.overlaps(around))
                .map(|way| (way.id, &way.line))
                .collect();
            let barred: Vec<Convex> = yards
                .iter()
                .filter(|(bounds, _)| bounds.overlaps(around.grown(60.0)))
                .map(|(_, shape)| shape.clone())
                .collect();
            *laid = Some(village::lay_out(key, village, (&streets, &barred), ground));
        });
        for (_, laid) in jobs {
            let village = laid.ok_or(Error::Shape)??;
            for plot in village.plots.iter().map(|plot| &plot.outline).chain(village.green.as_ref()) {
                let bounds = plot.bounds().ok_or(Error::Shape)?;
                self.plotted.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                self.plotted.push((bounds, plot.clone()));
            }
            self.laid_villages.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
            self.laid_villages.push(village);
        }
        self.laid_villages.sort_unstable_by_key(|village| village.settled);
        Ok(())
    }

    /// Cut every holding about the region into its fields, and file the
    /// corridors that reach it.
    fn parcel(&mut self, ground: &dyn Ground, runner: &dyn JobRunner) -> Result<(), Error> {
        let key = self.key();
        let hold = self.regions.hold;
        let mut jobs: Vec<(usize, Outcome<Parcelled>)> = Vec::new();
        for (index, holding) in self.holdings.iter().enumerate() {
            if holding.farmed() && holding.bounds.overlaps(hold) {
                jobs.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                jobs.push((index, None));
            }
        }
        let (lattice, holdings, ways) = (&self.lattice, &self.holdings[..], &self.ways[..]);
        let (yards, plotted) = (&self.yards[..], &self.plotted[..]);
        for_each(runner, &mut jobs, &|(index, laid)| {
            *laid = Some(parcelled(key, lattice, &holdings[*index], (ways, yards, plotted), ground));
        });
        for (index, laid) in jobs {
            let laid = laid.ok_or(Error::Shape)??;
            let holding = &mut self.holdings[index];
            holding.parcels = Some(laid.parcels);
            holding.plots = laid.plots;
            holding.stretches = laid.stretches;
            holding.corridors = laid.corridors;
        }
        Ok(())
    }

    /// The boundaries of every holding about the region and of the ways
    /// beside them.
    fn bound(&mut self, ground: &dyn Ground, runner: &dyn JobRunner) -> Result<(), Error> {
        let key = self.key();
        let style = self.countryside.style;
        let (hold, bounded) = (self.regions.hold, self.regions.bounded);
        let mut jobs: Vec<(Job, Outcome<Vec<Boundary>>)> = Vec::new();
        for (index, holding) in self.holdings.iter().enumerate() {
            if holding.parcels.is_some() && holding.bounds.overlaps(hold) {
                jobs.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                jobs.push((Job::Holding(index), None));
            }
        }
        for (index, way) in self.ways.iter().enumerate() {
            if way.id.rank.bounded() && way.bounds.overlaps(bounded) {
                jobs.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                jobs.push((Job::Way(index), None));
            }
        }
        let land = Land {
            lattice: &self.lattice,
            holdings: &self.holdings,
            ways: &self.ways,
            ground,
        };
        let (holdings, ways, farmsteads) = (&self.holdings[..], &self.ways[..], &self.farmsteads[..]);
        let customs = |id: HoldingId| {
            holdings
                .binary_search_by(|holding| holding.id.cmp(&id))
                .ok()
                .and_then(|index| holdings.get(index))
                .map_or(Kind::Hedge, |holding| holding.custom)
        };
        for_each(runner, &mut jobs, &|(job, laid)| {
            *laid = Some(match *job {
                Job::Holding(index) => holding_bounds(key, (&style, &land), (&holdings[index], farmsteads)),
                Job::Way(index) => way_bounds(key, (&style, &land), &ways[index], &customs),
            });
        });
        let mut boundaries = Vec::new();
        for (_, found) in jobs {
            let found = found.ok_or(Error::Shape)??;
            boundaries.try_reserve(found.len()).map_err(|_| Error::OutOfMemory)?;
            boundaries.extend(found);
        }
        boundaries.sort_unstable_by_key(|boundary| boundary.id);
        self.boundaries = boundaries;
        Ok(())
    }

    /// The gateways every field about the region is entered by, and the
    /// stiles the paths cross its boundaries by.
    fn gate(&mut self, runner: &dyn JobRunner) -> Result<(), Error> {
        let key = self.key();
        let mut touching: Vec<(HoldingId, u32)> = Vec::new();
        for (index, boundary) in self.boundaries.iter().enumerate() {
            let index = u32::try_from(index).map_err(|_| Error::Shape)?;
            for side in [boundary.left, boundary.right] {
                if let Side::Field(field) = side {
                    touching.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                    touching.push((field.holding, index));
                }
            }
        }
        touching.sort_unstable();
        touching.dedup();
        let hold = self.regions.hold;
        let mut jobs: Vec<(HoldingId, usize, Outcome<Gaps>)> = Vec::new();
        for holding in self.holdings.iter().filter(|holding| holding.bounds.overlaps(hold)) {
            if let Some(parcels) = &holding.parcels {
                jobs.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                jobs.push((holding.id, parcels.fields.len(), None));
            }
        }
        let (boundaries, touching) = (&self.boundaries[..], &touching[..]);
        for_each(runner, &mut jobs, &|(holding, fields, gates)| {
            let from = touching.partition_point(|entry| entry.0 < *holding);
            let ours: Vec<u32> = touching[from..]
                .iter()
                .take_while(|entry| entry.0 == *holding)
                .map(|entry| entry.1)
                .collect();
            *gates = Some(boundary::gateways(key, (*holding, *fields), (boundaries, &ours)));
        });
        let mut paths: Vec<(Rect, &Line)> = Vec::new();
        for way in self.ways.iter().filter(|way| way.id.rank == Rank::Path) {
            paths.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
            paths.push((way.bounds, &way.line));
        }
        let mut crossings: Vec<(u32, Outcome<Gaps>)> = Vec::new();
        crossings
            .try_reserve_exact(self.boundaries.len())
            .map_err(|_| Error::OutOfMemory)?;
        for index in 0..self.boundaries.len() {
            crossings.push((u32::try_from(index).map_err(|_| Error::Shape)?, None));
        }
        for_each(runner, &mut crossings, &|(index, stiles)| {
            *stiles = boundaries
                .get(*index as usize)
                .map(|boundary| boundary::stiles(key, (*index, boundary), &paths));
        });
        let mut gaps = Vec::new();
        for laid in jobs
            .into_iter()
            .map(|(.., gates)| gates)
            .chain(crossings.into_iter().map(|(_, stiles)| stiles))
        {
            let laid = laid.ok_or(Error::Shape)??;
            gaps.try_reserve(laid.len()).map_err(|_| Error::OutOfMemory)?;
            gaps.extend(laid);
        }
        boundary::hang(&mut self.boundaries, gaps)
    }

    /// The layout of the region: what each of its fields is used for, and
    /// every feature reaching it.
    fn laid(&mut self, runner: &dyn JobRunner) -> Result<Layout, Error> {
        let region = self.region;
        let mut holdings: Vec<Holding> = Vec::new();
        for holding in core::mem::take(&mut self.holdings) {
            if (holding.parcels.is_some() || !holding.farmed()) && holding.bounds.overlaps(region) {
                holdings.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                holdings.push(holding);
            }
        }
        let parcels = self.used(&holdings, runner)?;
        let mut boundaries: Vec<Boundary> = Vec::new();
        for boundary in core::mem::take(&mut self.boundaries) {
            if Rect::of(boundary.line.iter().copied()).is_some_and(|bounds| bounds.overlaps(region)) {
                boundaries.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                boundaries.push(boundary);
            }
        }
        let mut farmsteads: Vec<Farmstead> = Vec::new();
        for (farm, (bounds, _)) in self.farmsteads.iter().zip(&self.yards) {
            if bounds.overlaps(region) {
                farmsteads.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                farmsteads.push(farm.clone());
            }
        }
        let mut villages: Vec<Village> = Vec::new();
        for village in core::mem::take(&mut self.laid_villages) {
            let reaches = village
                .plots
                .iter()
                .map(|plot| &plot.outline)
                .chain(village.green.as_ref())
                .any(|shape| shape.bounds().is_some_and(|bounds| bounds.overlaps(region)));
            if reaches {
                villages.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                villages.push(village);
            }
        }
        let mut settlements: Vec<Settlement> = Vec::new();
        settlements
            .try_reserve_exact(farmsteads.len() + villages.len())
            .map_err(|_| Error::OutOfMemory)?;
        settlements.extend(farmsteads.iter().map(|farm| Settlement {
            settled: farm.settled,
            at: farm.at,
        }));
        settlements.extend(villages.iter().filter_map(|village| self.village(village.settled).copied()));
        settlements.sort_unstable_by_key(|settlement| settlement.settled);
        let (ways, reaching) = self.kept_ways(&mut holdings)?;
        Ok(Layout {
            region,
            lattice: self.lattice,
            ways,
            reaching,
            holdings,
            parcels,
            boundaries,
            settlements,
            farmsteads,
            villages,
        })
    }

    /// The fields of `holdings` reaching the region, each with what it is
    /// used for, in their order.
    fn used(&self, holdings: &[Holding], runner: &dyn JobRunner) -> Result<Vec<Parcel>, Error> {
        let key = self.key();
        let (mix, warm) = (self.countryside.mix, self.countryside.warm);
        let mut parcels: Vec<(Option<Point>, Parcel)> = Vec::new();
        for holding in holdings {
            let farm = holding
                .farm
                .and_then(|index| self.farmsteads.get(index as usize))
                .map(|farm| farm.at)
                .or(holding.home);
            for field in holding.parcels.iter().flat_map(|parcels| &parcels.fields) {
                if field.cell.bounds().is_some_and(|bounds| bounds.overlaps(self.region)) {
                    parcels.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                    parcels.push((
                        farm,
                        Parcel {
                            field: field.clone(),
                            usage: Usage {
                                used: usage::Use::Overgrown,
                                bale: None,
                            },
                        },
                    ));
                }
            }
        }
        for_each(runner, &mut parcels, &|(farm, parcel)| {
            parcel.usage = usage::usage(key, &mix, &parcel.field, (*farm, warm));
        });
        let mut used: Vec<Parcel> = Vec::new();
        used.try_reserve_exact(parcels.len()).map_err(|_| Error::OutOfMemory)?;
        used.extend(parcels.into_iter().map(|(_, parcel)| parcel));
        used.sort_unstable_by_key(|parcel| parcel.field.id);
        Ok(used)
    }

    /// The ways reaching the region, and those whose corridors `holdings`
    /// file, the files renumbered to match; and which of them reach it.
    fn kept_ways(&self, holdings: &mut [Holding]) -> Result<(Vec<Way>, Vec<u32>), Error> {
        let mut kept: Vec<u32> = Vec::new();
        for (index, way) in self.ways.iter().enumerate() {
            if way.bounds.overlaps(self.region) {
                kept.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                kept.push(u32::try_from(index).map_err(|_| Error::Shape)?);
            }
        }
        for holding in holdings.iter() {
            kept.try_reserve(holding.stretches.len()).map_err(|_| Error::OutOfMemory)?;
            kept.extend(holding.stretches.iter().map(|entry| entry.0));
        }
        kept.sort_unstable();
        kept.dedup();
        for holding in holdings.iter_mut() {
            for entry in &mut holding.stretches {
                let renumbered = kept.binary_search(&entry.0).ok().and_then(|at| u32::try_from(at).ok());
                entry.0 = renumbered.ok_or(Error::Shape)?;
            }
        }
        let mut ways: Vec<Way> = Vec::new();
        ways.try_reserve_exact(kept.len()).map_err(|_| Error::OutOfMemory)?;
        let mut reaching = Vec::new();
        for &index in &kept {
            let way = self.ways.get(index as usize).ok_or(Error::Shape)?;
            if way.bounds.overlaps(self.region) {
                reaching.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                reaching.push(u32::try_from(ways.len()).map_err(|_| Error::Shape)?);
            }
            ways.push(way.clone());
        }
        Ok((ways, reaching))
    }
}

/// The way of `rank` between `a` and `b`, each what it is and where it lies,
/// run from the lesser end as its identity orders them.
fn between(rank: Rank, a: (Placed, Point), b: (Placed, Point)) -> Planned {
    let ((first, from), (second, to)) = if a.0 <= b.0 { (a, b) } else { (b, a) };
    Planned {
        id: WayId {
            rank,
            joins: network::ordered(first, second),
        },
        ends: (from, to),
    }
}

/// A holding cut into its fields: its parcels, the plots reaching it, and
/// the ways' stretches whose corridors reach it, filed.
struct Parcelled {
    parcels: Parcels,
    plots: Vec<Convex>,
    stretches: Vec<(u32, u32)>,
    corridors: Buckets,
}

/// `holding` cut into its fields among `ways` and the farmsteads' `yards` and
/// villages' plots `plotted` about it, over `ground`.
fn parcelled(
    key: Key,
    lattice: &Lattice,
    holding: &Holding,
    (ways, yards, plotted): (&[Way], &Shapes, &Shapes),
    ground: &dyn Ground,
) -> Result<Parcelled, Error> {
    let extent = holding.bounds.grown(field::CELL);
    let mut reaching: Vec<(u32, Rank, &Line)> = Vec::new();
    for (index, way) in ways.iter().enumerate() {
        if way.id.rank.bounded() && way.bounds.overlaps(extent) {
            reaching.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
            reaching.push((u32::try_from(index).map_err(|_| Error::Shape)?, way.id.rank, &way.line));
        }
    }
    let mut plots: Vec<Convex> = Vec::new();
    for (bounds, shape) in yards.iter().chain(plotted) {
        if bounds.overlaps(extent) {
            plots.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
            plots.push(shape.clone());
        }
    }
    let mut lines: Vec<(Rank, &Line)> = Vec::new();
    lines.try_reserve_exact(reaching.len()).map_err(|_| Error::OutOfMemory)?;
    lines.extend(reaching.iter().map(|&(_, rank, line)| (rank, line)));
    let surround = Surround {
        ways: &lines,
        plots: &plots,
        ground,
    };
    let parcels = field::parcels(key, lattice, holding.id, &surround)?;
    let (stretches, corridors) = filed_corridors(extent, &reaching)?;
    Ok(Parcelled {
        parcels,
        plots,
        stretches,
        corridors,
    })
}

/// A unit of a layout's boundaries: one holding's own, or one way's sides.
#[derive(Copy, Clone, Debug)]
enum Job {
    Holding(usize),
    Way(usize),
}

/// The boundaries `holding` lays: its cuts, the edges it shares with greater
/// holdings, and its farmstead's yard.
fn holding_bounds(
    key: Key,
    (style, land): (&Style, &Land<'_>),
    (holding, farmsteads): (&Holding, &[Farmstead]),
) -> Result<Vec<Boundary>, Error> {
    let Some(parcels) = holding.parcels.as_ref() else {
        return Ok(Vec::new());
    };
    let own = boundary::Holding {
        id: holding.id,
        outline: &holding.outline,
        parcels,
        custom: holding.custom,
    };
    let mut boundaries = boundary::cuts(key, (style, land), &own)?;
    let farms = |id: HoldingId| land.holding(id).is_none_or(Holding::farmed);
    let edges = boundary::edges(key, (style, land), (&own, &farms))?;
    boundaries.try_reserve(edges.len()).map_err(|_| Error::OutOfMemory)?;
    boundaries.extend(edges);
    if let Some(farm) = holding.farm.and_then(|index| farmsteads.get(index as usize)) {
        let yard = boundary::yard(key, (style, land), (&own, &farm.plot))?;
        boundaries.try_reserve(yard.len()).map_err(|_| Error::OutOfMemory)?;
        boundaries.extend(yard);
    }
    Ok(boundaries)
}

/// The boundaries lining `way`, and across its end where it ends among
/// fields.
fn way_bounds(
    key: Key,
    (style, land): (&Style, &Land<'_>),
    way: &Way,
    customs: &(dyn Fn(HoldingId) -> Kind + Sync),
) -> Result<Vec<Boundary>, Error> {
    let mut boundaries = boundary::sides(key, (style, land), (way.id, &way.line), customs)?;
    let ends_among_fields = matches!(way.id.joins, Joins::Ends(_, Placed::Gateway(_, gateway)) if gateway > 0);
    if way.id.rank == Rank::Track && ends_among_fields {
        if let Some(end) = boundary::end(key, (style, land), (way.id, &way.line), customs)? {
            boundaries.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
            boundaries.push(end);
        }
    }
    Ok(boundaries)
}

/// One unit of `active`'s route: its lattice marked, a unit of its search,
/// or its line laid; its line once laid.
fn advance(
    active: &mut Active,
    (key, ground): (Key, &dyn Ground),
    (ways, barred, plotted): (&[Way], &Shapes, &Shapes),
) -> Result<Option<Line>, Error> {
    let rank = active.planned.id.rank;
    if active.routing.is_none() {
        active.routing = Some(Routing::new(rank, active.planned.ends)?);
    }
    let Some(routing) = active.routing.as_mut() else {
        return Err(Error::Shape);
    };
    if routing.prepared() {
        return routing.step(key, ground);
    }
    let rect = routing.rect();
    // A way follows the greater ways its lattice reaches, but for the tracks,
    // which nothing follows.
    let follows = |other: Rank| other < rank && other != Rank::Track;
    let mut greater: Vec<(Rank, &Line)> = Vec::new();
    for way in ways.iter().filter(|way| follows(way.id.rank) && way.bounds.overlaps(rect)) {
        greater.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
        greater.push((way.id.rank, &way.line));
    }
    let plots: &Shapes = if matches!(rank, Rank::Track | Rank::Path) { plotted } else { &[] };
    let mut shapes: Vec<Convex> = Vec::new();
    for (bounds, shape) in barred.iter().chain(plots) {
        if bounds.overlaps(rect) {
            shapes.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
            shapes.push(shape.clone());
        }
    }
    routing.prepare(&greater, &shapes).map(|()| None)
}

/// The stretches of `ways` — each by its index among the layout's ways —
/// whose corridors reach `extent`, and those stretches filed by where their
/// corridors reach.
fn filed_corridors(extent: Rect, ways: &[(u32, Rank, &Line)]) -> Result<(Vec<(u32, u32)>, Buckets), Error> {
    let mut stretches = Vec::new();
    let mut filed = Buckets::over(extent, BUCKET);
    for &(index, rank, line) in ways {
        let verge = rank.laying().verge;
        for (segment, pair) in line.stations.windows(2).enumerate() {
            let (a, b) = (pair[0], pair[1]);
            let reach = 0.5 * a.width.max(b.width) + verge;
            let Some(bounds) = Rect::of([a.at, b.at]).map(|rect| rect.grown(reach)) else {
                continue;
            };
            if !bounds.overlaps(extent) {
                continue;
            }
            let item = u32::try_from(stretches.len()).map_err(|_| Error::Shape)?;
            stretches.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
            stretches.push((index, u32::try_from(segment).map_err(|_| Error::Shape)?));
            filed.file(bounds, item)?;
        }
    }
    filed.sort();
    Ok((stretches, filed))
}

/// A count as `f64`.
#[allow(
    clippy::cast_precision_loss,
    reason = "a holding's gateways are counted far within an f64's whole numbers"
)]
fn real(count: usize) -> f64 {
    count as f64
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;

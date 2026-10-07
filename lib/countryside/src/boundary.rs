//! What bounds a field or lines a way — a hedge, a wall, a fence, a ditch, or
//! nothing — drawn by the ground it runs over and a region's custom, each
//! holding keeping mostly to its own; the gateway each field is entered by,
//! and the stiles a path crosses by.
//!
//! A boundary runs wherever a field meets what is not that field: another
//! field across a cut or a holding's edge, a bounded way's side or a track's
//! end, or a farmstead's yard. Each line is read along its length for what
//! lies either side, and a boundary runs while that holds. Water and a
//! village's plots bound a field by themselves.

use core::hash::Hasher;

use alloc::vec::Vec;

use tairix_util::mathf;

use crate::field::{FieldId, Node, Parcels};
use crate::ground::Lie;
use crate::holding::{HoldingId, AROUND};
use crate::key::{Draws, Key, Stage};
use crate::network::{Rank, WayId};
use crate::plane::{self, Convex, Point, Rect};
use crate::route::Line;
use crate::Error;

/// What a boundary is.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Kind {
    /// A hedge, its standards among it.
    Hedge,
    /// A wall of stone laid dry.
    Wall,
    /// A fence of posts and rails.
    Fence,
    /// A ditch cut along the boundary.
    Ditch,
    /// Nothing: land open to the land beside it.
    Open,
}

impl Kind {
    /// The least and the most a boundary of this kind kept as it should be
    /// stands over the ground: a wall to the top of its cap, a hedge cut to a
    /// height, a fence to its posts' tops; nought for a ditch or nothing.
    #[must_use]
    pub const fn stands(self) -> (f64, f64) {
        match self {
            Self::Wall => (1.25, 1.7),
            Self::Hedge => (1.7, 2.6),
            Self::Fence => (1.12, 1.32),
            Self::Ditch | Self::Open => (0.0, 0.0),
        }
    }

    /// How tall a boundary of this kind stands, drawn from `draws`: as it is
    /// kept, but for a hedge left to grow out now and then.
    fn height(self, draws: &mut Draws) -> f64 {
        let (low, high) = self.stands();
        match self {
            Self::Hedge if !draws.chance(0.7) => draws.range(high, GROWN_OUT),
            Self::Wall | Self::Hedge | Self::Fence => draws.range(low, high),
            Self::Ditch | Self::Open => 0.0,
        }
    }

    /// How much of the wind blowing across a boundary of this kind passes
    /// through it: none through a wall, about half through a hedge, most
    /// between a fence's rails, all where nothing stands.
    #[must_use]
    pub const fn porosity(self) -> f64 {
        match self {
            Self::Wall => 0.0,
            Self::Hedge => 0.5,
            Self::Fence => 0.8,
            Self::Ditch | Self::Open => 1.0,
        }
    }
}

/// How tall a hedge left to grow out stands at most.
const GROWN_OUT: f64 = 3.8;

/// What lies to one side of a boundary.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Side {
    /// A field.
    Field(FieldId),
    /// A bounded way's corridor.
    Way(WayId),
    /// A settlement's plot.
    Plot,
    /// Water.
    Water,
    /// Land too little to be a field, left to grow over.
    Waste,
    /// Land beyond what the layout knows.
    Out,
}

/// Which side of its way a way's boundary lines.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Hand {
    /// Its left, looking along it from its first end.
    Left,
    /// Its right.
    Right,
}

/// What a boundary bounds, and so who lays it.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Owner {
    /// A cut between two of a holding's fields: the holding, and the cut's
    /// node among its parcels.
    Cut(HoldingId, u32),
    /// Edge `n` of a holding, anticlockwise from its first corner, laid by the
    /// lesser of the two holdings it parts, or by the greater where nobody
    /// farms the lesser.
    Edge(HoldingId, u8),
    /// One side of a bounded way where it runs beside a holding's fields.
    Side(WayId, HoldingId, Hand),
    /// Across a track's last end, where it ends among a holding's fields.
    End(WayId, HoldingId),
    /// About a holding's farmstead.
    Yard(HoldingId),
}

impl Owner {
    /// Write what names it into `hasher`.
    fn write(self, hasher: &mut impl Hasher) {
        match self {
            Self::Cut(holding, node) => {
                hasher.write_u8(0);
                holding.write(hasher);
                hasher.write_u32(node);
            }
            Self::Edge(holding, edge) => {
                hasher.write_u8(1);
                holding.write(hasher);
                hasher.write_u8(edge);
            }
            Self::Side(way, holding, hand) => {
                hasher.write_u8(2);
                way.write(hasher);
                holding.write(hasher);
                hasher.write_u8(hand as u8);
            }
            Self::End(way, holding) => {
                hasher.write_u8(3);
                way.write(hasher);
                holding.write(hasher);
            }
            Self::Yard(holding) => {
                hasher.write_u8(4);
                holding.write(hasher);
            }
        }
    }
}

/// Which boundary a boundary is: what it bounds, and its place among the
/// runs of that.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct BoundaryId {
    /// What it bounds.
    pub owner: Owner,
    /// Its place among the boundaries of that, in the order they run.
    pub run: u32,
}

impl BoundaryId {
    /// The word every draw for it is keyed from.
    fn word(self, key: Key) -> u64 {
        let mut hasher = key.hasher(Stage::Boundary);
        self.owner.write(&mut hasher);
        hasher.write_u32(self.run);
        hasher.finish()
    }
}

/// What a gap in a boundary lets through.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Through {
    /// A field's gateway, a gate hung in it.
    Gateway,
    /// A path, over a stile.
    Path,
}

/// A gap in a boundary.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Gap {
    /// How far along the boundary its middle lies.
    pub along: f64,
    /// How wide it is.
    pub width: f64,
    /// What it lets through.
    pub through: Through,
    /// The word its gate or stile is drawn from.
    pub key: u64,
}

/// A boundary, from its line's first point to its last.
#[derive(Clone, Debug, PartialEq)]
pub struct Boundary {
    /// Which it is.
    pub id: BoundaryId,
    /// What it is.
    pub kind: Kind,
    /// Its line.
    pub line: Vec<Point>,
    /// What lies to its left, looking along its line.
    pub left: Side,
    /// What lies to its right.
    pub right: Side,
    /// The gaps left in it, in order along it.
    pub gaps: Vec<Gap>,
    /// How tall it stands over the ground, as [`Kind`] draws it.
    pub height: f64,
    /// The word everything drawn of it is keyed from.
    pub key: u64,
}

/// How much a region's boundaries are of each kind, weighed against each
/// other: its consumer's custom, before the ground has its say.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Style {
    /// Hedges'.
    pub hedge: f64,
    /// Walls'.
    pub wall: f64,
    /// Fences'.
    pub fence: f64,
    /// Ditches'.
    pub ditch: f64,
    /// Nothing's.
    pub open: f64,
}

impl Style {
    /// Each kind's weight where the ground lies as `lie` has it: walls where
    /// stone is at hand, ditches where the ground is wet, fences near woods,
    /// hedges on deep soil.
    fn weighed(&self, lie: Lie) -> [(Kind, f64); 5] {
        [
            (
                Kind::Hedge,
                self.hedge
                    * (1.0 - 0.75 * lie.stony)
                    * (1.0 - 0.4 * lie.wet)
                    * (0.4 + 0.6 * lie.fertile),
            ),
            (Kind::Wall, self.wall * (0.1 + 1.6 * lie.stony)),
            (Kind::Fence, self.fence * (0.35 + lie.wooded)),
            (Kind::Ditch, self.ditch * 3.0 * lie.wet * lie.wet),
            (Kind::Open, self.open),
        ]
    }

    /// The kind a boundary over ground lying `lie` is, its holding keeping to
    /// `custom` wherever the ground lets it.
    pub(crate) fn kind(&self, lie: Lie, custom: Kind, draws: &mut Draws) -> Kind {
        let weights = self.weighed(lie);
        let total: f64 = weights.iter().map(|(_, weight)| weight.max(0.0)).sum();
        let customary = weights
            .iter()
            .find(|(kind, _)| *kind == custom)
            .map_or(0.0, |(_, weight)| weight.max(0.0));
        if total > 0.0 && draws.chance(0.75 * mathf::sqrt(customary / total)) {
            return custom;
        }
        draws.pick(&weights).unwrap_or(custom)
    }

    /// The kind a holding over ground lying `lie` keeps to: any but none.
    pub(crate) fn custom(&self, lie: Lie, draws: &mut Draws) -> Kind {
        let mut weights = self.weighed(lie);
        weights[4].1 = 0.0;
        draws.pick(&weights).unwrap_or(Kind::Hedge)
    }
}

/// What a layout knows of the land, as its boundaries read it.
pub(crate) trait Survey: Sync {
    /// What lies at `at`.
    fn side(&self, at: Point) -> Side;
    /// What the ground is like at `at`.
    fn lie(&self, at: Point) -> Lie;
}

/// How far apart a line is read for what lies either side of it, how many
/// halvings find where that changes, how far to either side it is read, and
/// the shortest boundary kept.
const SAMPLE: f64 = 2.0;
const HALVINGS: u32 = 5;
const OFF: f64 = 0.5;
const SHORTEST: f64 = 1.5;

/// A stretch of a line over which one pair of sides holds.
struct Run {
    line: Vec<Point>,
    left: Side,
    right: Side,
}

/// The runs of `line` over which what `sides` reads — at a point, given the
/// line's unit way there — is a pair `keeps` keeps, each from where the pair
/// begins to where it ends.
fn runs(
    line: &[Point],
    sides: &dyn Fn(Point, Point) -> (Side, Side),
    keeps: &dyn Fn(Side, Side) -> bool,
) -> Result<Vec<Run>, Error> {
    let mut runs = Vec::new();
    let mut open: Option<Run> = None;
    let mut last: Option<(Point, (Side, Side))> = None;
    for pair in line.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let span = (b - a).length();
        if !span.is_finite() || span <= 0.0 {
            continue;
        }
        let way = (b - a) * (1.0 / span);
        let count = mathf::round_i32(mathf::ceil(span / SAMPLE)).max(1);
        for step in 0..=count {
            let at = a.lerp(b, f64::from(step) / f64::from(count));
            let here = sides(at, way);
            let begun = match last {
                Some((_, held)) if held == here => None,
                Some((before, held)) => {
                    let (mut low, mut high) = (before, at);
                    for _ in 0..HALVINGS {
                        let middle = low.lerp(high, 0.5);
                        if sides(middle, way) == held {
                            low = middle;
                        } else {
                            high = middle;
                        }
                    }
                    let change = low.lerp(high, 0.5);
                    close(&mut runs, open.take(), change)?;
                    Some(change)
                }
                None => Some(at),
            };
            if let Some(begun) = begun {
                open = if keeps(here.0, here.1) {
                    let mut line = Vec::new();
                    line.try_reserve(4).map_err(|_| Error::OutOfMemory)?;
                    line.push(begun);
                    Some(Run {
                        line,
                        left: here.0,
                        right: here.1,
                    })
                } else {
                    None
                };
            }
            last = Some((at, here));
        }
        if let Some(run) = &mut open {
            extend(&mut run.line, b)?;
        }
    }
    if let Some((at, _)) = last {
        close(&mut runs, open, at)?;
    }
    Ok(runs)
}

/// `at` added to `line` unless it already ends there.
fn extend(line: &mut Vec<Point>, at: Point) -> Result<(), Error> {
    if line.last() != Some(&at) {
        line.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
        line.push(at);
    }
    Ok(())
}

/// `run`, if there is one, ended at `at` and kept if it is long enough.
fn close(runs: &mut Vec<Run>, run: Option<Run>, at: Point) -> Result<(), Error> {
    let Some(mut run) = run else {
        return Ok(());
    };
    extend(&mut run.line, at)?;
    if plane::length(&run.line) >= SHORTEST {
        runs.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
        runs.push(run);
    }
    Ok(())
}

/// What lies either side of the line through `at` running `way`: left, then
/// right.
fn either(survey: &dyn Survey, at: Point, way: Point) -> (Side, Side) {
    let off = way.left() * OFF;
    (survey.side(at + off), survey.side(at - off))
}

/// The boundary `run` is, as `id` names it: its kind drawn by the ground at
/// its middle and `custom`, or nothing at all by `open`'s chance.
fn laid(
    key: Key,
    (style, survey): (&Style, &dyn Survey),
    (id, run): (BoundaryId, Run),
    (custom, open): (Kind, f64),
) -> Boundary {
    let word = id.word(key);
    let mut draws = key.draws_for(Stage::Boundary, word);
    let length = plane::length(&run.line);
    let middle = plane::at(&run.line, 0.5 * length).map_or(Point::default(), |(at, _)| at);
    let kind = if draws.chance(open) {
        Kind::Open
    } else {
        style.kind(survey.lie(middle), custom, &mut draws)
    };
    Boundary {
        id,
        kind,
        line: run.line,
        left: run.left,
        right: run.right,
        gaps: Vec::new(),
        height: kind.height(&mut draws),
        key: word,
    }
}

/// `boundary` added to `boundaries`, or the refusal.
fn push(boundaries: &mut Vec<Boundary>, boundary: Boundary) -> Result<(), Error> {
    boundaries.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
    boundaries.push(boundary);
    Ok(())
}

/// What a holding's own boundaries are laid from: which it is, its outline
/// and parcels, and the kind it keeps to.
pub(crate) struct Holding<'a> {
    pub(crate) id: HoldingId,
    pub(crate) outline: &'a Convex,
    pub(crate) parcels: &'a Parcels,
    pub(crate) custom: Kind,
}

/// The boundaries along the cuts between `holding`'s fields.
pub(crate) fn cuts(
    key: Key,
    (style, survey): (&Style, &dyn Survey),
    holding: &Holding<'_>,
) -> Result<Vec<Boundary>, Error> {
    let mut boundaries = Vec::new();
    let parcels = holding.parcels;
    for (node, entry) in parcels.nodes.iter().enumerate() {
        let Node::Cut { chord, .. } = *entry else {
            continue;
        };
        let node = u32::try_from(node).map_err(|_| Error::Shape)?;
        let block = parcels.block_of(node);
        let ours = |field: FieldId| field.holding == holding.id && parcels.block(field) == block;
        let keeps = |left: Side, right: Side| match (left, right) {
            (Side::Field(a), Side::Field(b)) => a != b && ours(a) && ours(b),
            _ => false,
        };
        let found = runs(
            &[chord.0, chord.1],
            &|at, way| either(survey, at, way),
            &keeps,
        )?;
        for (run, found) in found.into_iter().enumerate() {
            let id = BoundaryId {
                owner: Owner::Cut(holding.id, node),
                run: u32::try_from(run).map_err(|_| Error::Shape)?,
            };
            push(
                &mut boundaries,
                laid(key, (style, survey), (id, found), (holding.custom, 0.0)),
            )?;
        }
    }
    Ok(boundaries)
}

/// The boundaries along the edges `holding` lays — those it shares with a
/// greater holding, or with one nobody `farms` — where a field of either
/// meets a field or waste of the other.
pub(crate) fn edges(
    key: Key,
    (style, survey): (&Style, &dyn Survey),
    (holding, farms): (&Holding<'_>, &dyn Fn(HoldingId) -> bool),
) -> Result<Vec<Boundary>, Error> {
    let mut boundaries = Vec::new();
    let corners = &holding.outline.corners;
    for edge in 0..corners.len() {
        let (di, dj) = AROUND[(edge + 1) % AROUND.len()];
        let beyond = HoldingId::new(holding.id.i + di, holding.id.j + dj);
        if beyond < holding.id && farms(beyond) {
            continue;
        }
        let line = [corners[edge], corners[(edge + 1) % corners.len()]];
        let keeps = |inner: Side, outer: Side| match (inner, outer) {
            (Side::Field(a), Side::Field(b)) => a.holding != b.holding,
            (Side::Field(_), Side::Waste) | (Side::Waste, Side::Field(_)) => true,
            _ => false,
        };
        let found = runs(&line, &|at, way| either(survey, at, way), &keeps)?;
        for (run, found) in found.into_iter().enumerate() {
            let id = BoundaryId {
                owner: Owner::Edge(holding.id, u8::try_from(edge).map_err(|_| Error::Shape)?),
                run: u32::try_from(run).map_err(|_| Error::Shape)?,
            };
            push(
                &mut boundaries,
                laid(key, (style, survey), (id, found), (holding.custom, 0.0)),
            )?;
        }
    }
    Ok(boundaries)
}

/// The boundaries about `holding`'s farmstead's plot `plot`, where a field
/// meets it.
pub(crate) fn yard(
    key: Key,
    (style, survey): (&Style, &dyn Survey),
    (holding, plot): (&Holding<'_>, &Convex),
) -> Result<Vec<Boundary>, Error> {
    let mut boundaries = Vec::new();
    let mut line = Vec::new();
    line.try_reserve_exact(plot.corners.len() + 1)
        .map_err(|_| Error::OutOfMemory)?;
    line.extend_from_slice(&plot.corners);
    line.extend(plot.corners.first().copied());
    // Its corners run anticlockwise, so the plot lies to the left, a way
    // through it at times.
    let sides = |at: Point, way: Point| either(survey, at, way);
    let keeps = |inner: Side, outer: Side| {
        matches!((inner, outer), (Side::Plot | Side::Way(_), Side::Field(_)))
    };
    for (run, found) in runs(&line, &sides, &keeps)?.into_iter().enumerate() {
        let id = BoundaryId {
            owner: Owner::Yard(holding.id),
            run: u32::try_from(run).map_err(|_| Error::Shape)?,
        };
        push(
            &mut boundaries,
            laid(key, (style, survey), (id, found), (holding.custom, 0.0)),
        )?;
    }
    Ok(boundaries)
}

/// How likely a way of `rank` runs unbounded by its fields at any stretch:
/// a track across a farm's own land most, a lane hardly ever.
const fn unbounded(rank: Rank) -> f64 {
    match rank {
        Rank::Highway => 0.03,
        Rank::Road => 0.1,
        Rank::Lane => 0.05,
        Rank::Track => 0.35,
        Rank::Path => 1.0,
    }
}

/// The boundaries lining the bounded way `way`, each where one side runs
/// beside one holding's fields, its kind kept to that holding's `customs`.
pub(crate) fn sides(
    key: Key,
    (style, survey): (&Style, &dyn Survey),
    (way, line): (WayId, &Line),
    customs: &dyn Fn(HoldingId) -> Kind,
) -> Result<Vec<Boundary>, Error> {
    let mut boundaries = Vec::new();
    let verge = way.rank.laying().verge;
    let mut runs_of: Vec<(HoldingId, Hand, u32)> = Vec::new();
    for hand in [Hand::Left, Hand::Right] {
        let sign = if hand == Hand::Left { 1.0 } else { -1.0 };
        let side = offset(line, sign, verge)?;
        let reads = |at: Point, heading: Point| {
            let beyond = survey.side(at + heading.left() * (sign * OFF));
            if hand == Hand::Left {
                (beyond, Side::Way(way))
            } else {
                (Side::Way(way), beyond)
            }
        };
        let keeps = |left: Side, right: Side| {
            matches!(
                if hand == Hand::Left { left } else { right },
                Side::Field(_)
            )
        };
        for found in runs(&side, &reads, &keeps)? {
            let ((Side::Field(field), _) | (_, Side::Field(field))) = (found.left, found.right)
            else {
                continue;
            };
            let holding = field.holding;
            let run = if let Some((_, _, count)) = runs_of
                .iter_mut()
                .find(|(h, s, _)| *h == holding && *s == hand)
            {
                *count += 1;
                *count
            } else {
                runs_of.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                runs_of.push((holding, hand, 0));
                0
            };
            let id = BoundaryId {
                owner: Owner::Side(way, holding, hand),
                run,
            };
            push(
                &mut boundaries,
                laid(
                    key,
                    (style, survey),
                    (id, found),
                    (customs(holding), unbounded(way.rank)),
                ),
            )?;
        }
    }
    Ok(boundaries)
}

/// The line `sign` of `line`'s way — left for one, right for minus one — at
/// half its breadth and its verge: where the boundaries that line it run.
/// A point that would turn back on the one before, on the inside of a bend
/// tighter than the offset, is left out.
fn offset(line: &Line, sign: f64, verge: f64) -> Result<Vec<Point>, Error> {
    let stations = &line.stations;
    let mut side: Vec<Point> = Vec::new();
    side.try_reserve_exact(stations.len())
        .map_err(|_| Error::OutOfMemory)?;
    for (index, station) in stations.iter().enumerate() {
        let before = stations[index.saturating_sub(1)].at;
        let after = stations[(index + 1).min(stations.len() - 1)].at;
        let way = (after - before).normalized();
        let at = station.at + way.left() * (sign * (0.5 * station.width + verge));
        if side.last().is_none_or(|&last| (at - last).dot(way) > 0.0) {
            side.push(at);
        }
    }
    Ok(side)
}

/// The boundary across the last end of the track `way`, where it ends among
/// fields, its gateway in its middle: the field beyond is entered there.
pub(crate) fn end(
    key: Key,
    (style, survey): (&Style, &dyn Survey),
    (id, line): (WayId, &Line),
    customs: &dyn Fn(HoldingId) -> Kind,
) -> Result<Option<Boundary>, Error> {
    let [.., before, last] = line.stations.as_slice() else {
        return Ok(None);
    };
    let forward = (last.at - before.at).normalized();
    let half = 0.5 * last.width + id.rank.laying().verge;
    let middle = last.at + forward * (0.6 * half);
    let across = forward.left();
    let reads = |at: Point, way: Point| (survey.side(at + way.left() * OFF), Side::Way(id));
    let keeps = |beyond: Side, _: Side| matches!(beyond, Side::Field(_));
    let found = runs(
        &[middle + across * half, middle - across * half],
        &reads,
        &keeps,
    )?;
    let Some(found) = found
        .into_iter()
        .max_by(|a, b| plane::length(&a.line).total_cmp(&plane::length(&b.line)))
    else {
        return Ok(None);
    };
    let Side::Field(field) = found.left else {
        return Ok(None);
    };
    let length = plane::length(&found.line);
    let boundary_id = BoundaryId {
        owner: Owner::End(id, field.holding),
        run: 0,
    };
    let mut boundary = laid(
        key,
        (style, survey),
        (boundary_id, found),
        (customs(field.holding), 0.0),
    );
    let mut draws = key.draws_for(Stage::Gate, boundary.key);
    let width = (last.width + 0.6).min(length - 0.4);
    if width > 1.0 {
        boundary
            .gaps
            .try_reserve(1)
            .map_err(|_| Error::OutOfMemory)?;
        boundary.gaps.push(Gap {
            along: 0.5 * length,
            width,
            through: Through::Gateway,
            key: draws.word(),
        });
    }
    Ok(Some(boundary))
}

/// The gateway of a field fronting `boundary`: a gate's breadth, kept clear
/// of its ends where it is long enough; `None` where it is too short to
/// hang one in.
fn gateway(key: Key, boundary: &Boundary) -> Option<Gap> {
    let length = plane::length(&boundary.line);
    let mut draws = key.draws_for(Stage::Gate, boundary.key);
    let width = draws.range(3.6, 4.6);
    if length < width + 1.0 {
        return None;
    }
    let clear = 3.0 + 0.5 * width;
    let along = if length >= 2.0 * clear {
        draws.range(clear, length - clear)
    } else {
        0.5 * length
    };
    Some(Gap {
        along,
        width,
        through: Through::Gateway,
        key: draws.word(),
    })
}

/// How readily a field is entered from what a boundary parts it from: its
/// yard first, then a track, a lane, a road and a highway; `None` for what
/// a field is not entered from.
fn entered_from(owner: Owner, holding: HoldingId) -> Option<u8> {
    match owner {
        Owner::Yard(yard) if yard == holding => Some(0),
        Owner::Side(way, ..) => match way.rank {
            Rank::Track => Some(1),
            Rank::Lane => Some(2),
            Rank::Road => Some(3),
            Rank::Highway => Some(4),
            Rank::Path => None,
        },
        Owner::Cut(..) | Owner::Edge(..) | Owner::End(..) | Owner::Yard(_) => None,
    }
}

/// The gateways `holding`'s `fields` fields are entered by, among the
/// boundaries `touching` them of `boundaries`, each as the boundary it is
/// hung in and the gap it leaves. A field is entered from the end of a track,
/// else from the yard or way it fronts, else from the neighbour by which its
/// holding's other fields reach one, else across its holding's edges.
pub(crate) fn gateways(
    key: Key,
    (holding, fields): (HoldingId, usize),
    (boundaries, touching): (&[Boundary], &[u32]),
) -> Result<Vec<(u32, Gap)>, Error> {
    let entry = Entry {
        key,
        holding,
        fields,
        boundaries,
        touching,
    };
    let mut reached = tairix_util::fallible::filled(fields, false).ok_or(Error::OutOfMemory)?;
    let mut gates = Vec::new();
    entry.fronting(&mut reached, &mut gates)?;
    entry.inward(&mut reached, &mut gates)?;
    entry.across(&reached, &mut gates)?;
    Ok(gates)
}

/// What a holding's gateways are found among.
struct Entry<'a> {
    key: Key,
    holding: HoldingId,
    fields: usize,
    boundaries: &'a [Boundary],
    touching: &'a [u32],
}

impl Entry<'_> {
    /// The index of the holding's field on `side`, where one is.
    fn ours(&self, side: Side) -> Option<usize> {
        match side {
            Side::Field(field) if field.holding == self.holding => usize::try_from(field.index)
                .ok()
                .filter(|&index| index < self.fields),
            _ => None,
        }
    }

    /// The boundaries touching the holding's fields, each with its index.
    fn touching(&self) -> impl Iterator<Item = (u32, &Boundary)> + '_ {
        self.touching
            .iter()
            .filter_map(|&index| Some((index, self.boundaries.get(index as usize)?)))
    }

    /// A gateway hung in boundary `index`, where it is long enough for one.
    fn hung(&self, index: u32, gates: &mut Vec<(u32, Gap)>) -> Result<bool, Error> {
        let Some(gap) = self
            .boundaries
            .get(index as usize)
            .and_then(|boundary| gateway(self.key, boundary))
        else {
            return Ok(false);
        };
        gates.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
        gates.push((index, gap));
        Ok(true)
    }

    /// Enter every field from the track ending in it, else from the best of
    /// the yard and ways it fronts.
    fn fronting(&self, reached: &mut [bool], gates: &mut Vec<(u32, Gap)>) -> Result<(), Error> {
        let mut best: Vec<Option<(u8, f64, BoundaryId, u32)>> =
            tairix_util::fallible::filled(self.fields, None).ok_or(Error::OutOfMemory)?;
        for (index, boundary) in self.touching() {
            let Some(field) = self
                .ours(boundary.left)
                .or_else(|| self.ours(boundary.right))
            else {
                continue;
            };
            if matches!(boundary.id.owner, Owner::End(..)) {
                reached[field] = true;
                continue;
            }
            let Some(preference) = entered_from(boundary.id.owner, self.holding) else {
                continue;
            };
            let candidate = (
                preference,
                plane::length(&boundary.line),
                boundary.id,
                index,
            );
            if best[field].is_none_or(|held| {
                (candidate.0, -candidate.1, candidate.2) < (held.0, -held.1, held.2)
            }) {
                best[field] = Some(candidate);
            }
        }
        for (field, best) in best.into_iter().enumerate() {
            if let (false, Some((.., index))) = (reached[field], best) {
                reached[field] = self.hung(index, gates)?;
            }
        }
        Ok(())
    }

    /// Enter the fields still not entered from their neighbours, outward
    /// from those that are a layer at a time, each by its longest boundary
    /// with the layer before that will hang a gate.
    fn inward(&self, reached: &mut [bool], gates: &mut Vec<(u32, Gap)>) -> Result<(), Error> {
        let mut cuts: Vec<(usize, usize, f64, BoundaryId, u32)> = Vec::new();
        for (index, boundary) in self.touching() {
            if let (Owner::Cut(..), Some(a), Some(b)) = (
                boundary.id.owner,
                self.ours(boundary.left),
                self.ours(boundary.right),
            ) {
                cuts.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                cuts.push((a, b, plane::length(&boundary.line), boundary.id, index));
            }
        }
        // Longest first, so each field's first boundary in the layer is its
        // longest.
        cuts.sort_unstable_by(|a, b| b.2.total_cmp(&a.2).then(a.3.cmp(&b.3)));
        loop {
            let mut layer: Vec<(usize, u32)> = Vec::new();
            for &(a, b, _, _, index) in &cuts {
                let to = match (reached[a], reached[b]) {
                    (true, false) => b,
                    (false, true) => a,
                    _ => continue,
                };
                layer.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                layer.push((to, index));
            }
            let mut entered = false;
            let mut chosen: Vec<usize> = Vec::new();
            for (field, index) in layer {
                if reached[field] || chosen.contains(&field) {
                    continue;
                }
                if self.hung(index, gates)? {
                    chosen.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                    chosen.push(field);
                    entered = true;
                }
            }
            for field in chosen {
                reached[field] = true;
            }
            if !entered {
                return Ok(());
            }
        }
    }

    /// Enter each field its holding cannot reach from over every edge of
    /// the holding it meets another field across, by its longest boundary
    /// there.
    fn across(&self, reached: &[bool], gates: &mut Vec<(u32, Gap)>) -> Result<(), Error> {
        let mut across: Vec<(usize, Owner, f64, BoundaryId, u32)> = Vec::new();
        for (index, boundary) in self.touching() {
            let Owner::Edge(..) = boundary.id.owner else {
                continue;
            };
            let (field, other) = match (self.ours(boundary.left), self.ours(boundary.right)) {
                (Some(field), None) => (field, boundary.right),
                (None, Some(field)) => (field, boundary.left),
                _ => continue,
            };
            if reached[field] || !matches!(other, Side::Field(_)) {
                continue;
            }
            let length = plane::length(&boundary.line);
            let owner = boundary.id.owner;
            match across
                .iter_mut()
                .find(|entry| entry.0 == field && entry.1 == owner)
            {
                Some(entry) if (-length, boundary.id) < (-entry.2, entry.3) => {
                    *entry = (field, owner, length, boundary.id, index);
                }
                Some(_) => {}
                None => {
                    across.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                    across.push((field, owner, length, boundary.id, index));
                }
            }
        }
        across.sort_unstable_by_key(|entry| (entry.0, entry.1));
        for (.., index) in across {
            self.hung(index, gates)?;
        }
        Ok(())
    }
}

/// The stiles the paths `paths` cross `boundary` by, the `index`th of the
/// boundaries: one wherever a path's line crosses its line.
pub(crate) fn stiles(
    key: Key,
    (index, boundary): (u32, &Boundary),
    paths: &[(Rect, &Line)],
) -> Result<Vec<(u32, Gap)>, Error> {
    let mut stiles = Vec::new();
    let Some(bounds) = Rect::of(boundary.line.iter().copied()) else {
        return Ok(stiles);
    };
    for (path_bounds, line) in paths {
        if !path_bounds.overlaps(bounds) {
            continue;
        }
        for pair in line.stations.windows(2) {
            let (a, b) = (pair[0].at, pair[1].at);
            if Rect::of([a, b]).is_none_or(|segment| !segment.overlaps(bounds)) {
                continue;
            }
            let mut walked = 0.0;
            for edge in boundary.line.windows(2) {
                let (c, d) = (edge[0], edge[1]);
                let span = (d - c).length();
                if let Some((_, share)) = plane::crossing((a, b), (c, d)) {
                    let along = walked + share * span;
                    let mut hasher = key.hasher(Stage::Gate);
                    hasher.write_u64(boundary.key);
                    hasher.write_i32(mathf::round_i32(along * 100.0));
                    stiles.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                    stiles.push((
                        index,
                        Gap {
                            along,
                            width: 1.0,
                            through: Through::Path,
                            key: hasher.finish(),
                        },
                    ));
                }
                walked += span;
            }
        }
    }
    Ok(stiles)
}

/// Hang `gaps`, each the index of its boundary among `boundaries` and the
/// gap it leaves, gateways first: a gap overlapping one already hung is
/// left out, and each boundary's gaps end in order along it.
pub(crate) fn hang(boundaries: &mut [Boundary], mut gaps: Vec<(u32, Gap)>) -> Result<(), Error> {
    gaps.sort_unstable_by(|(a, x), (b, y)| {
        a.cmp(b)
            .then((x.through == Through::Path).cmp(&(y.through == Through::Path)))
            .then(x.along.total_cmp(&y.along))
            .then(x.key.cmp(&y.key))
    });
    for (index, gap) in gaps {
        let Some(boundary) = boundaries.get_mut(index as usize) else {
            continue;
        };
        let clashes = boundary.gaps.iter().any(|hung| {
            (hung.along - gap.along).abs() < f64::midpoint(hung.width, gap.width) + 0.5
        });
        if !clashes {
            boundary
                .gaps
                .try_reserve(1)
                .map_err(|_| Error::OutOfMemory)?;
            boundary.gaps.push(gap);
        }
    }
    for boundary in boundaries {
        boundary
            .gaps
            .sort_unstable_by(|a, b| a.along.total_cmp(&b.along));
    }
    Ok(())
}

/// The stretches of `from`..`to` along a boundary that its `gaps`, in order
/// along it, leave standing, each at least `least` long.
///
/// # Errors
///
/// [`Error::OutOfMemory`] when the heap will not hold them.
pub fn standing(
    gaps: &[Gap],
    (from, to): (f64, f64),
    least: f64,
) -> Result<Vec<(f64, f64)>, Error> {
    let mut stretches = Vec::new();
    let mut start = from;
    let mut keep = |stretch: (f64, f64)| -> Result<(), Error> {
        stretches.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
        stretches.push(stretch);
        Ok(())
    };
    for gap in gaps {
        let (open, close) = (gap.along - 0.5 * gap.width, gap.along + 0.5 * gap.width);
        if close <= start || open >= to {
            continue;
        }
        if open - start >= least {
            keep((start, open))?;
        }
        start = close;
    }
    if to - start >= least {
        keep((start, to))?;
    }
    Ok(stretches)
}

#[cfg(test)]
#[path = "boundary_tests.rs"]
mod tests;

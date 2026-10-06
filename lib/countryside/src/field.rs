//! A holding's fields: the land between its ways, water and plots, cut
//! again and again across its longest reach, in the frame its contour or the
//! way beside it sets, until each piece is the size its ground makes a field.
//!
//! The holding is read on a raster of cells: a cell is land, water, a way's
//! corridor or a settlement's plot, and the land falls into blocks the ways
//! and water part. Each block is cut by straight lines, so every field is the
//! block's land within one convex cell of the holding.

use alloc::vec::Vec;

use tairix_util::mathf;

use crate::ground::{self, Ground};
use crate::holding::{HoldingId, Lattice};
use crate::key::{Key, Stage};
use crate::network::Rank;
use crate::plane::{Convex, Point};
use crate::route::Line;
use crate::Error;

/// How far apart a holding's raster cells lie.
pub(crate) const CELL: f64 = 4.0;

/// The least land a field holds; a block of less is waste, left to grow over.
const LEAST_FIELD: f64 = 2500.0;

/// What a cell of a holding's raster is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Cell {
    /// Beyond the holding.
    Out,
    Water,
    /// A bounded way's corridor.
    Way,
    /// A settlement's plot.
    Plot,
    /// Land, in block `n`; `u16::MAX` while its block is not yet numbered.
    Land(u16),
    /// Land too little to be a field.
    Waste,
}

/// Land whose block is not yet numbered.
const UNNUMBERED: Cell = Cell::Land(u16::MAX);

/// Which field a field is: its holding and its place in the holding's order.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct FieldId {
    /// The holding it lies in.
    pub holding: HoldingId,
    /// Its place among the holding's fields.
    pub index: u32,
}

/// One of a holding's fields.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    /// Which field it is.
    pub id: FieldId,
    /// The convex cell of the holding it is the land within.
    pub cell: Convex,
    /// How much land it holds, in square metres.
    pub area: f64,
    /// Its land's middle.
    pub middle: Point,
    /// The way its length runs, as a unit way.
    pub along: Point,
    /// The block of its holding's land it was cut from.
    pub(crate) block: u16,
    /// How steeply its land lies, as rise over run, on the whole.
    pub slope: f64,
    /// The unit way its land falls; nought where it lies level.
    pub fall: Point,
    /// How wet its land lies, on the whole.
    pub wet: f64,
}

/// A cut of a block: the line through `at` across `normal`, the land behind
/// it — where `(p - at) · normal <= 0` — then the land ahead; and the chord
/// of the line across the piece it cut.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum Node {
    Cut {
        at: Point,
        normal: Point,
        behind: u32,
        ahead: u32,
        chord: (Point, Point),
    },
    Field(u32),
}

/// A holding's raster and the cuts its blocks' fields were made by.
#[derive(Clone, Debug)]
pub(crate) struct Parcels {
    /// The raster's first cell's low corner, and its cells across and down.
    pub(crate) origin: Point,
    pub(crate) columns: usize,
    pub(crate) rows: usize,
    pub(crate) cells: Vec<Cell>,
    /// Each block's first node.
    pub(crate) roots: Vec<u32>,
    pub(crate) nodes: Vec<Node>,
    pub(crate) fields: Vec<Field>,
}

/// What a holding is laid out among: the bounded ways about it, its ground,
/// and the plots kept out of its fields.
pub(crate) struct Surround<'a> {
    pub(crate) ways: &'a [(Rank, &'a Line)],
    pub(crate) plots: &'a [Convex],
    pub(crate) ground: &'a dyn Ground,
}

impl Parcels {
    /// The field of block `block` whose convex cell holds `at`.
    pub(crate) fn field_in(&self, block: u16, at: Point) -> Option<u32> {
        let mut node = *self.roots.get(usize::from(block))?;
        loop {
            match *self.nodes.get(node as usize)? {
                Node::Field(field) => return Some(field),
                Node::Cut {
                    at: on,
                    normal,
                    behind,
                    ahead,
                    ..
                } => node = if (at - on).dot(normal) <= 0.0 { behind } else { ahead },
            }
        }
    }

    /// The block whose cuts node `node` is among.
    pub(crate) fn block_of(&self, node: u32) -> Option<u16> {
        let after = self.roots.partition_point(|&root| root <= node);
        u16::try_from(after.checked_sub(1)?).ok()
    }

    /// The block `field` was cut from, where it is one of these parcels'.
    pub(crate) fn block(&self, field: FieldId) -> Option<u16> {
        Some(self.fields.get(usize::try_from(field.index).ok()?)?.block)
    }

    /// The cell of the raster at `column` and `row`; `None` beyond it.
    pub(crate) fn cell(&self, column: usize, row: usize) -> Option<Cell> {
        (column < self.columns && row < self.rows)
            .then(|| self.cells.get(row * self.columns + column).copied())
            .flatten()
    }

    /// The column and row of the cell `at` lies in; `None` beyond the raster.
    pub(crate) fn position(&self, at: Point) -> Option<(usize, usize)> {
        let (column, row) = ((at.x - self.origin.x) / CELL, (at.y - self.origin.y) / CELL);
        if !(column >= 0.0 && row >= 0.0) {
            return None;
        }
        let (column, row) = (whole(column), whole(row));
        (column < self.columns && row < self.rows).then_some((column, row))
    }

    /// The middle of the cell at `column` and `row`.
    pub(crate) fn middle(&self, column: usize, row: usize) -> Point {
        centre(self.origin, column, row)
    }

    /// The middle of cell `index`.
    fn middle_of(&self, index: usize) -> Point {
        centre(self.origin, index % self.columns.max(1), index / self.columns.max(1))
    }
}

/// The middle of the cell at `column` and `row` of a raster from `origin`.
fn centre(origin: Point, column: usize, row: usize) -> Point {
    origin + Point::new((real(column) + 0.5) * CELL, (real(row) + 0.5) * CELL)
}

/// A count of cells from a share of their spacing, floored.
fn whole(value: f64) -> usize {
    usize::try_from(mathf::round_i32(mathf::floor(value).clamp(0.0, 2.0e9))).unwrap_or(0)
}

/// A count as `f64`.
#[allow(
    clippy::cast_precision_loss,
    reason = "a holding's cells are counted far within an f64's whole numbers"
)]
fn real(count: usize) -> f64 {
    count as f64
}

/// The fields of `holding`, among what `surround` holds about it; `Err`
/// where the heap will not hold them.
pub(crate) fn parcels(
    key: Key,
    lattice: &Lattice,
    holding: HoldingId,
    surround: &Surround<'_>,
) -> Result<Parcels, Error> {
    let outline = lattice.outline(holding);
    let bounds = outline.bounds().ok_or(Error::Shape)?;
    let origin = Point::new(
        mathf::floor(bounds.low.x / CELL) * CELL,
        mathf::floor(bounds.low.y / CELL) * CELL,
    );
    let columns = whole((bounds.high.x - origin.x) / CELL) + 1;
    let rows = whole((bounds.high.y - origin.y) / CELL) + 1;
    let mut cells = filled(columns * rows, Cell::Out)?;
    // Every cell the outline reaches is read, its middle within or not, so
    // no place within the holding lies in a cell it does not read.
    let half_diagonal = CELL * core::f64::consts::FRAC_1_SQRT_2;
    for row in 0..rows {
        for column in 0..columns {
            let at = centre(origin, column, row);
            if outline.distance(at) > half_diagonal {
                continue;
            }
            cells[row * columns + column] = if ground::wet(surround.ground, at) {
                Cell::Water
            } else if surround.plots.iter().any(|plot| plot.contains(at)) {
                Cell::Plot
            } else {
                UNNUMBERED
            };
        }
    }
    corridors(&mut cells, (origin, columns, rows), surround.ways);
    let blocks = blocks(&mut cells, columns)?;
    let count = blocks.len();
    let mut parcels = Parcels {
        origin,
        columns,
        rows,
        cells,
        roots: Vec::new(),
        nodes: Vec::new(),
        fields: Vec::new(),
    };
    parcels
        .roots
        .try_reserve_exact(count)
        .map_err(|_| Error::OutOfMemory)?;
    for (block, members) in blocks.into_iter().enumerate() {
        let block = u16::try_from(block).map_err(|_| Error::Shape)?;
        let root = cut(&mut parcels, (key, holding, block), (&outline, members), surround)?;
        parcels.roots.push(root);
    }
    Ok(parcels)
}

/// Mark every cell a bounded way's corridor reaches: within its breadth and
/// verges, and the half diagonal of a cell more, so no land either side of it
/// touches the other's by a corner.
fn corridors(cells: &mut [Cell], (origin, columns, rows): (Point, usize, usize), ways: &[(Rank, &Line)]) {
    let half_diagonal = CELL * core::f64::consts::FRAC_1_SQRT_2 + 1e-6;
    for (rank, line) in ways {
        if !rank.bounded() {
            continue;
        }
        let verge = rank.laying().verge;
        for pair in line.stations.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let reach = 0.5 * a.width.max(b.width) + verge + half_diagonal;
            let span = |low: f64, high: f64, origin: f64, count: usize| {
                let from = whole((low - reach - origin) / CELL);
                let to = whole((high + reach - origin) / CELL).min(count.saturating_sub(1));
                from..=to
            };
            for row in span(a.at.y.min(b.at.y), a.at.y.max(b.at.y), origin.y, rows) {
                for column in span(a.at.x.min(b.at.x), a.at.x.max(b.at.x), origin.x, columns) {
                    let at = centre(origin, column, row);
                    if crate::plane::onto_segment(at, a.at, b.at).1 < reach * reach {
                        if let Some(cell) = cells.get_mut(row * columns + column) {
                            if matches!(*cell, Cell::Land(_)) {
                                *cell = Cell::Way;
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Number the land's blocks: every run of land joined edge to edge, in the
/// order its first cell comes; a block too small to be a field left waste.
/// Each block's cells.
fn blocks(cells: &mut [Cell], columns: usize) -> Result<Vec<Vec<usize>>, Error> {
    let mut count = 0u16;
    let mut stack = Vec::new();
    let mut blocks: Vec<Vec<usize>> = Vec::new();
    for start in 0..cells.len() {
        if cells[start] != UNNUMBERED {
            continue;
        }
        cells[start] = Cell::Land(count);
        let mut members = Vec::new();
        stack.clear();
        stack.try_reserve(64).map_err(|_| Error::OutOfMemory)?;
        stack.push(start);
        while let Some(index) = stack.pop() {
            members.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
            members.push(index);
            let (column, row) = (index % columns, index / columns);
            let beside = [
                (column > 0).then(|| index - 1),
                (column + 1 < columns).then(|| index + 1),
                (row > 0).then(|| index - columns),
                Some(index + columns).filter(|&below| below < cells.len()),
            ];
            for next in beside.into_iter().flatten() {
                if cells[next] == UNNUMBERED {
                    cells[next] = Cell::Land(count);
                    stack.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
                    stack.push(next);
                }
            }
        }
        if real(members.len()) * CELL * CELL < LEAST_FIELD {
            for &index in &members {
                cells[index] = Cell::Waste;
            }
        } else {
            count = count
                .checked_add(1)
                .filter(|&count| count < u16::MAX)
                .ok_or(Error::Shape)?;
            blocks.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
            blocks.push(members);
        }
    }
    Ok(blocks)
}

/// One part of a block being cut: its convex cell of the holding, and the
/// raster cells of the block's land within it.
struct Piece {
    cell: Convex,
    members: Vec<usize>,
}

/// Cut block `block` of `holding`, its cells `members`, into fields, its
/// nodes and fields added to `parcels`; the block's first node.
fn cut(
    parcels: &mut Parcels,
    (key, holding, block): (Key, HoldingId, u16),
    (outline, members): (&Convex, Vec<usize>),
    surround: &Surround<'_>,
) -> Result<u32, Error> {
    let root = u32::try_from(parcels.nodes.len()).map_err(|_| Error::Shape)?;
    push(&mut parcels.nodes, Node::Field(u32::MAX))?;
    let mut pending = Vec::new();
    pending
        .try_reserve(32)
        .map_err(|_| Error::OutOfMemory)?;
    pending.push((
        root,
        Piece {
            cell: outline.clone(),
            members,
        },
        0u32,
    ));
    while let Some((node, piece, depth)) = pending.pop() {
        let middle = centre_of(parcels, &piece.members);
        let (slope, rise) = ground::slope(surround.ground, middle, 12.0);
        let lie = surround.ground.lie(middle);
        let area = real(piece.members.len()) * CELL * CELL;
        let place = (
            i64::from(holding.i) * 4096 + i64::from(block),
            i64::from(holding.j) * 4096 + i64::from(node),
        );
        let mut draws = key.draws(Stage::Cut, place);
        let wanted = field_size(slope, lie) * draws.range(0.7, 1.4);
        let frame = frame_of((slope, rise), middle, surround.ways, &mut draws);
        let split = (area > wanted && depth < 24)
            .then(|| split(parcels, &piece, frame, &mut draws))
            .flatten();
        let Some((at, normal, behind, ahead)) = split else {
            let index = u32::try_from(parcels.fields.len()).map_err(|_| Error::Shape)?;
            parcels.nodes[node as usize] = Node::Field(index);
            let along = long_axis(parcels, &piece.members, frame);
            parcels
                .fields
                .try_reserve(1)
                .map_err(|_| Error::OutOfMemory)?;
            parcels.fields.push(Field {
                id: FieldId { holding, index },
                cell: piece.cell,
                area,
                middle,
                along,
                block,
                slope,
                fall: -rise,
                wet: lie.wet,
            });
            continue;
        };
        let chord = piece.cell.chord(at, normal).unwrap_or((at, at));
        let behind_node = u32::try_from(parcels.nodes.len()).map_err(|_| Error::Shape)?;
        push(&mut parcels.nodes, Node::Field(u32::MAX))?;
        push(&mut parcels.nodes, Node::Field(u32::MAX))?;
        parcels.nodes[node as usize] = Node::Cut {
            at,
            normal,
            behind: behind_node,
            ahead: behind_node + 1,
            chord,
        };
        pending
            .try_reserve(2)
            .map_err(|_| Error::OutOfMemory)?;
        pending.push((behind_node + 1, ahead, depth + 1));
        pending.push((behind_node, behind, depth + 1));
    }
    Ok(root)
}

/// The size a field of ground lying `slope` and `lie` is cut to: smaller on
/// good, level ground worked closely, larger on steep, stony or wet ground.
fn field_size(slope: f64, lie: ground::Lie) -> f64 {
    let rough = 1.0 + 3.0 * slope.min(0.5) + 1.2 * lie.stony + 0.8 * lie.wet;
    32_000.0 * rough * (1.25 - 0.5 * lie.fertile)
}

/// The frame a piece is cut in: its first axis along the contour where the
/// ground falls enough to set one, along the greatest way beside it where
/// it does not, else a way drawn for it; each turned a little, as a field's
/// lines never run quite true.
fn frame_of(
    (slope, rise): (f64, Point),
    middle: Point,
    ways: &[(Rank, &Line)],
    draws: &mut crate::key::Draws,
) -> Point {
    let lean = draws.range(-0.14, 0.14);
    let along = if slope > 0.03 {
        rise.left()
    } else {
        ways.iter()
            .filter(|(rank, _)| rank.bounded())
            .filter_map(|(rank, line)| {
                let (near, _) = line.nearest(middle)?;
                let ahead = line.stations.get(near.segment + 1)?.at;
                let here = line.stations.get(near.segment)?.at;
                Some((near.distance + 40.0 * f64::from(*rank as u8), (ahead - here).normalized()))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map_or_else(|| Point::toward(draws.range(0.0, core::f64::consts::PI)), |(_, way)| way)
    };
    let (cos, sin) = (mathf::cos(lean), mathf::sin(lean));
    Point::new(along.x * cos - along.y * sin, along.x * sin + along.y * cos)
}

/// Where a piece is cut, if it can be: across whichever of its frame's axes
/// it reaches furthest along, near the middle of its land along that axis;
/// the line's point and normal, and the two pieces either side.
fn split(
    parcels: &Parcels,
    piece: &Piece,
    along: Point,
    draws: &mut crate::key::Draws,
) -> Option<(Point, Point, Piece, Piece)> {
    let across = along.left();
    let place = |index: usize| parcels.middle_of(index);
    let reach = |axis: Point| {
        piece
            .members
            .iter()
            .map(|&index| place(index).dot(axis))
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), v| (low.min(v), high.max(v)))
    };
    let (along_reach, across_reach) = (reach(along), reach(across));
    let normal = if along_reach.1 - along_reach.0 >= across_reach.1 - across_reach.0 {
        along
    } else {
        across
    };
    let mut projected = Vec::new();
    projected.try_reserve_exact(piece.members.len()).ok()?;
    projected.extend(piece.members.iter().map(|&index| place(index).dot(normal)));
    projected.sort_unstable_by(f64::total_cmp);
    // The cut runs between two cells' middles, never through one, so every
    // cell lies plainly to one side of it.
    let share = draws.range(0.38, 0.62);
    let from = whole(share * real(projected.len())).clamp(1, projected.len().saturating_sub(1));
    let after = (from..projected.len()).find(|&index| projected[index] > projected[index - 1])?;
    let at = normal * f64::midpoint(projected[after - 1], projected[after]);
    let (mut behind, mut ahead) = (Vec::new(), Vec::new());
    behind.try_reserve(piece.members.len()).ok()?;
    ahead.try_reserve(piece.members.len()).ok()?;
    for &index in &piece.members {
        if (place(index) - at).dot(normal) <= 0.0 {
            behind.push(index);
        } else {
            ahead.push(index);
        }
    }
    let least = |cells: &Vec<usize>| real(cells.len()) * CELL * CELL >= LEAST_FIELD;
    if !least(&behind) || !least(&ahead) {
        return None;
    }
    Some((
        at,
        normal,
        Piece {
            cell: piece.cell.behind(at, normal)?,
            members: behind,
        },
        Piece {
            cell: piece.cell.behind(at, -normal)?,
            members: ahead,
        },
    ))
}

/// The middle of the cells `members`.
fn centre_of(parcels: &Parcels, members: &[usize]) -> Point {
    let sum = members.iter().fold(Point::default(), |sum, &index| {
        sum + (parcels.middle_of(index) - parcels.origin)
    });
    parcels.origin + sum * (1.0 / real(members.len().max(1)))
}

/// The way a field's land runs longest, of its frame's two axes.
fn long_axis(parcels: &Parcels, members: &[usize], along: Point) -> Point {
    let reach = |axis: Point| {
        let (low, high) = members.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), &index| {
            let at = Point::new(real(index % parcels.columns), real(index / parcels.columns)).dot(axis);
            (low.min(at), high.max(at))
        });
        high - low
    };
    if reach(along) >= reach(along.left()) {
        along
    } else {
        along.left()
    }
}

/// `count` copies of `value`, or the refusal.
fn filled<T: Clone>(count: usize, value: T) -> Result<Vec<T>, Error> {
    tairix_util::fallible::filled(count, value).ok_or(Error::OutOfMemory)
}

/// `value` pushed onto `values`, or the refusal.
fn push<T>(values: &mut Vec<T>, value: T) -> Result<(), Error> {
    values.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
    values.push(value);
    Ok(())
}

#[cfg(test)]
#[path = "field_tests.rs"]
mod tests;

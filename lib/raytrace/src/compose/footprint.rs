//! What stands on a stage's ground, as circles a new piece keeps clear of.
//!
//! A land's woods hold hundreds of thousands, standing anywhere out to the
//! horizon, so each circle is chained into the cells of a grid held in blocks
//! keyed by block rather than laid over one square: a question asks only the
//! cells about it, wherever on the ground it is asked, and the grid grows a
//! block at a time, never moving the cells it already holds.
//!
//! A circle is taken either by a piece standing there or as ground the
//! composition keeps open of pieces — the eye's own, a pond — where what
//! grows wild may still stand.

use alloc::vec::Vec;

use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;
use tairix_util::{fallible, mathf};

/// The clearance two pieces' circles keep between them.
pub(super) const GAP: f64 = 0.08;

/// How broad a cell of the grid is: a tree's spacing, so a question about a
/// trunk asks a cell or four.
const CELL: f64 = 4.0;

/// How many cells a side a block of the grid holds, its cells' chains begun
/// together the first time a circle reaches it.
const BLOCK: i32 = 32;

/// No link: a chain's end.
const END: u32 = u32::MAX;

/// Circles on the ground, each taken by a piece or kept open, chained into
/// every cell its square, widened by half the gap, covers: two circles too
/// close together have squares that overlap, so they share a cell.
#[derive(Debug)]
pub(super) struct Footprints {
    circles: Vec<Circle>,
    /// Where each block's cells' first links lie among `heads`. The blocks
    /// are the scene's own places, never a caller's, so the unkeyed hash
    /// serves.
    blocks: HashMap<(i32, i32), u32, BuildFastHash>,
    /// The first link of each cell's chain, a block's cells row by row.
    heads: Vec<Vec<u32>>,
    /// A circle, and the next link along its cell's chain.
    links: Vec<(u32, u32)>,
}

impl Default for Footprints {
    fn default() -> Self {
        Self {
            circles: Vec::new(),
            blocks: HashMap::with_hasher(BuildFastHash::new()),
            heads: Vec::new(),
            links: Vec::new(),
        }
    }
}

/// What takes a circle of the ground.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Taken {
    /// A piece stands there: a trunk, a stone, a wall.
    Piece,
    /// The composition keeps it open of pieces.
    Open,
}

#[derive(Copy, Clone, Debug)]
struct Circle {
    at: (f64, f64),
    radius: f64,
    taken: Taken,
}

/// The block holding cell `(column, row)`, and the cell's place within it.
fn place_of((column, row): (i32, i32)) -> ((i32, i32), usize) {
    let block = (column.div_euclid(BLOCK), row.div_euclid(BLOCK));
    let within = row.rem_euclid(BLOCK) * BLOCK + column.rem_euclid(BLOCK);
    (block, usize::try_from(within).unwrap_or(0))
}

/// The cells the square `half` either way of `at` covers, as the columns and
/// the rows from and to.
fn covered((x, z): (f64, f64), half: f64) -> ((i32, i32), (i32, i32)) {
    let cell = |value: f64| mathf::round_i32(mathf::floor(value / CELL));
    (
        (cell(x - half), cell(x + half)),
        (cell(z - half), cell(z + half)),
    )
}

impl Footprints {
    /// Whether a piece `radius` across at `at` keeps clear of every circle;
    /// a negative radius takes no room of its own.
    pub(super) fn clear(&self, at: (f64, f64), radius: f64) -> bool {
        self.clear_of(at, radius, |_| true)
    }

    /// Whether what grows `radius` across at `at` keeps clear of every
    /// piece, open ground being no bar to it.
    pub(super) fn clear_of_pieces(&self, at: (f64, f64), radius: f64) -> bool {
        self.clear_of(at, radius, |taken| taken == Taken::Piece)
    }

    fn clear_of(&self, at: (f64, f64), radius: f64, bars: impl Fn(Taken) -> bool) -> bool {
        let radius = radius.max(0.0);
        let apart = |id: u32| {
            self.circles.get(id as usize).is_none_or(|circle| {
                let (dx, dz) = (at.0 - circle.at.0, at.1 - circle.at.1);
                let least = radius + circle.radius + GAP;
                !bars(circle.taken) || dx * dx + dz * dz > least * least
            })
        };
        let (columns, rows) = covered(at, radius + 0.5 * GAP);
        (rows.0..=rows.1).all(|row| {
            (columns.0..=columns.1).all(|column| {
                let mut link = self.head((column, row));
                while let Some(&(id, next)) = self.links.get(link as usize) {
                    if !apart(id) {
                        return false;
                    }
                    link = next;
                }
                true
            })
        })
    }

    /// Take a circle `radius` across at `at`, no room for a negative one;
    /// `None` when the heap will not hold it.
    pub(super) fn claim(&mut self, at: (f64, f64), radius: f64, taken: Taken) -> Option<()> {
        let radius = radius.max(0.0);
        let id = u32::try_from(self.circles.len()).ok()?;
        let (columns, rows) = covered(at, radius + 0.5 * GAP);
        let span =
            |(from, to): (i32, i32)| usize::try_from(i64::from(to) - i64::from(from) + 1).ok();
        let cells = span(columns)?.checked_mul(span(rows)?)?;
        // Every block it reaches is made before it is chained into any, so a
        // refusal leaves it chained nowhere.
        let blocks = |(from, to): (i32, i32)| from.div_euclid(BLOCK)..=to.div_euclid(BLOCK);
        for row in blocks(rows) {
            for column in blocks(columns) {
                self.block((column, row))?;
            }
        }
        self.circles.try_reserve(1).ok()?;
        self.links.try_reserve(cells).ok()?;
        self.circles.push(Circle { at, radius, taken });
        for row in rows.0..=rows.1 {
            for column in columns.0..=columns.1 {
                let link = u32::try_from(self.links.len()).ok()?;
                let (block, within) = place_of((column, row));
                let head = self
                    .blocks
                    .get(&block)
                    .and_then(|&index| self.heads.get_mut(index as usize))
                    .and_then(|heads| heads.get_mut(within))?;
                self.links.push((id, *head));
                *head = link;
            }
        }
        Some(())
    }

    /// The first link of cell `cell`'s chain.
    fn head(&self, cell: (i32, i32)) -> u32 {
        let (block, within) = place_of(cell);
        self.blocks
            .get(&block)
            .and_then(|&index| self.heads.get(index as usize))
            .and_then(|heads| heads.get(within))
            .copied()
            .unwrap_or(END)
    }

    /// Where block `block`'s cells' first links lie among its heads, the
    /// block's chains begun empty the first time it is asked for; `None` when
    /// the heap will not hold them.
    fn block(&mut self, block: (i32, i32)) -> Option<u32> {
        if let Some(&index) = self.blocks.get(&block) {
            return Some(index);
        }
        let index = u32::try_from(self.heads.len()).ok()?;
        let cells = usize::try_from(BLOCK * BLOCK).ok()?;
        self.heads.try_reserve(1).ok()?;
        self.blocks.try_reserve(1).ok()?;
        self.heads.push(fallible::filled(cells, END)?);
        self.blocks.try_insert(block, index).ok()?;
        Some(index)
    }
}

#[cfg(all(test, feature = "scene-tests"))]
impl Footprints {
    /// How many circles are taken.
    pub(super) const fn len(&self) -> usize {
        self.circles.len()
    }

    /// Where the circle taken `index`th stands, and how far it reaches.
    pub(super) fn circle(&self, index: usize) -> Option<((f64, f64), f64)> {
        self.circles
            .get(index)
            .map(|circle| (circle.at, circle.radius))
    }
}

#[cfg(test)]
#[path = "footprint_tests.rs"]
mod tests;

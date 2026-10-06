//! What stands on a stage's ground, as circles a new piece keeps clear of.
//!
//! A land's woods hold hundreds of thousands, standing anywhere out to the
//! horizon, so each circle is chained into the cells of a grid keyed by cell
//! rather than laid over one square: a question asks only the cells about it,
//! wherever on the ground it is asked.
//!
//! A circle is taken either by a piece standing there or as ground the
//! composition keeps open of pieces — the eye's own, a pond — where what
//! grows wild may still stand.

use alloc::vec::Vec;

use tairix_collections::HashMap;
use tairix_hash::BuildFastHash;
use tairix_util::mathf;

/// The clearance two pieces' circles keep between them.
pub(super) const GAP: f64 = 0.08;

/// How broad a cell of the grid is: a tree's spacing, so a question about a
/// trunk asks a cell or four.
const CELL: f64 = 4.0;

/// No link: a chain's end.
const END: u32 = u32::MAX;

/// Circles on the ground, each taken by a piece or kept open, chained into
/// every cell its square, widened by half the gap, covers: two circles too
/// close together have squares that overlap, so they share a cell.
#[derive(Debug)]
pub(super) struct Footprints {
    circles: Vec<Circle>,
    /// The first link of each cell's chain. The cells are the scene's own
    /// places, never a caller's, so the unkeyed hash serves.
    heads: HashMap<(i32, i32), u32, BuildFastHash>,
    /// A circle, and the next link along its cell's chain.
    links: Vec<(u32, u32)>,
}

impl Default for Footprints {
    fn default() -> Self {
        Self {
            circles: Vec::new(),
            heads: HashMap::with_hasher(BuildFastHash::new()),
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
                let mut link = self.heads.get(&(column, row)).copied().unwrap_or(END);
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
        self.circles.try_reserve(1).ok()?;
        self.links.try_reserve(cells).ok()?;
        self.heads.try_reserve(cells).ok()?;
        self.circles.push(Circle { at, radius, taken });
        for row in rows.0..=rows.1 {
            for column in columns.0..=columns.1 {
                let link = u32::try_from(self.links.len()).ok()?;
                let head = self.heads.get(&(column, row)).copied().unwrap_or(END);
                self.links.push((id, head));
                self.heads.try_insert((column, row), link).ok()?;
            }
        }
        Some(())
    }
}

#[cfg(test)]
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

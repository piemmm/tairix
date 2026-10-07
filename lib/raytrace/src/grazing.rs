//! What stock leave on the pasture they graze: their pats, scattered where
//! they fed, and the rank grass about each they will not eat for weeks.
//!
//! Pats are hashed from a lattice over the land rather than kept, each cell
//! holding at most one, so the grass reads the pats it grows about from the
//! same draws the pats themselves are laid from, wherever either looks.

use tairix_util::mathf;

use crate::noise::{hash2, noise2, smoothstep};
use crate::sample::{mix32, unit};

/// The side of a cell of the lattice pats are hashed from, and the share of
/// cells holding one: about one pat to twenty square metres.
const CELL: f64 = 3.0;
const SHARE: f64 = 0.45;

/// The least and most a pat spreads across, in radius.
const RADIUS: (f64, f64) = (0.09, 0.16);

/// How far about a pat its grass stands rank, as a share of its radius:
/// stock shun some nine times its area. A pat lies far enough inside its
/// cell that its rank grass never leaves it, so a place need only look at
/// its own cell's pat.
pub(crate) const RANK: f64 = 3.0;
const _: () = assert!(2.0 * RANK * RADIUS.1 < CELL);

/// How far out from its middle a pat smothers the grass beneath it, as a
/// share of its radius: well inside even an old pat's ragged, sunken edge.
pub(crate) const SMOTHERED: f64 = 0.5;

/// How broad the patches stock gather and dung in are, in cells.
const GATHERED: f64 = 6.0;

/// A pat: where it lies, how far it spreads, how long it has lain — `0.0`
/// fresh to `1.0` crumbled into the grass — and its key.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Pat {
    pub(crate) at: (f64, f64),
    pub(crate) radius: f64,
    pub(crate) age: f64,
    pub(crate) key: u32,
}

/// The pat cell `(column, row)` holds under `seed`, if it holds one: more
/// where the stock gathered.
pub(crate) fn pat((column, row): (i32, i32), seed: u32) -> Option<Pat> {
    let key = hash2(
        column.cast_unsigned(),
        row.cast_unsigned(),
        seed ^ 0x7061_7473,
    );
    let gathered = 0.5
        + 0.5
            * noise2(
                f64::from(column) / GATHERED,
                f64::from(row) / GATHERED,
                seed ^ 0x6761,
            );
    if unit(key) >= SHARE * (0.3 + 1.4 * gathered) {
        return None;
    }
    let draw = |salt: u32| unit(mix32(key ^ salt));
    // Kept clear of its cell's walls by its rank grass's reach.
    let margin = RANK * RADIUS.1 / CELL + 0.02;
    let inside = |salt: u32| margin + (1.0 - 2.0 * margin).max(0.0) * draw(salt);
    // More old pats than fresh: each lies for weeks before it crumbles.
    let age = mathf::sqrt(draw(3));
    Some(Pat {
        at: (
            (f64::from(column) + inside(1)) * CELL,
            (f64::from(row) + inside(2)) * CELL,
        ),
        radius: RADIUS.0 + (RADIUS.1 - RADIUS.0) * draw(4),
        age,
        key,
    })
}

/// The cell of the pats' lattice `(x, z)` lies in.
fn cell_of((x, z): (f64, f64)) -> (i32, i32) {
    (
        mathf::round_i32(mathf::floor(x / CELL)),
        mathf::round_i32(mathf::floor(z / CELL)),
    )
}

impl Pat {
    /// How rank the grass stands at `at` about it, `0.0` grazed to `1.0` at
    /// its rankest; `None` beneath it, where nothing grows.
    pub(crate) fn rank_at(&self, at: (f64, f64)) -> Option<f64> {
        let off = mathf::hypot(at.0 - self.at.0, at.1 - self.at.1);
        if off < SMOTHERED * self.radius {
            return None;
        }
        // Stock shun its ground while it lies, so the grass there grows rank
        // over the weeks, until it crumbles and they graze it again.
        let shunned =
            smoothstep(0.0, 0.4, self.age) * (1.0 - 0.7 * smoothstep(0.65, 1.0, self.age));
        Some(shunned * (1.0 - smoothstep(self.radius, RANK * self.radius, off)))
    }
}

/// How rank the grass stands at `(x, z)` about the pats under `seed`, `0.0`
/// grazed to `1.0` at its rankest; `None` beneath a pat.
pub(crate) fn rank(at: (f64, f64), seed: u32) -> Option<f64> {
    pat(cell_of(at), seed).map_or(Some(0.0), |pat| pat.rank_at(at))
}

/// Every pat under `seed` lying within `reach` of `(x, z)`.
pub(crate) fn about((x, z): (f64, f64), reach: f64, seed: u32) -> impl Iterator<Item = Pat> {
    let (low, high) = (
        cell_of((x - reach, z - reach)),
        cell_of((x + reach, z + reach)),
    );
    (low.1..=high.1)
        .flat_map(move |row| (low.0..=high.0).map(move |column| (column, row)))
        .filter_map(move |cell| pat(cell, seed))
        .filter(move |pat| mathf::hypot(pat.at.0 - x, pat.at.1 - z) <= reach)
}

#[cfg(test)]
#[path = "grazing_tests.rs"]
mod tests;

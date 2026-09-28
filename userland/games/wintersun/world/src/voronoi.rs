//! The jittered-grid Voronoi partition the plates and the rock provinces are
//! both drawn from: one site per grid cell, displaced inside it, and every
//! point owned by its nearest site.

use tairix_util::mathf;

use crate::geom::signed;

/// Largest jitter of a site inside its grid cell, as a fraction of the cell.
/// Just under a half, so two sites can approach but never coincide and the
/// partition can never be degenerate.
pub const SITE_JITTER: f64 = 0.45;

/// Wrap a grid index onto `0..modulus`, flooring toward negative infinity so
/// `-1` maps to the last cell rather than to itself.
#[must_use]
pub fn wrap(index: i32, modulus: u32) -> i32 {
    index.rem_euclid(signed(modulus).max(1))
}

/// The `N` sites nearest `point`, nearest first, each with its squared
/// distance; `site_of(cx, cy)` is the site grid cell `(cx, cy)` holds and what
/// it stands for.
///
/// Exact. The nine cells around the point are searched first, and each ring
/// beyond them only while a site there could still be nearer than the `N`th
/// found: past [`SITE_JITTER`] of a third, a site two cells off can be the
/// nearest, but it lies at least `r + ½ − SITE_JITTER` beyond the point's
/// nearest cell edge for a ring `r + 1` cells out. Ties go to the site met
/// first, row by row from the innermost ring. A point that is not finite has
/// no nearest site, and is answered with its own cell's at the greatest
/// distance.
pub fn nearest<T: Copy, const N: usize>(
    point: (f64, f64),
    site_of: impl Fn(i32, i32) -> ((f64, f64), T),
) -> [(f64, T); N] {
    let (fx, fy) = (mathf::floor(point.0), mathf::floor(point.1));
    let (cx, cy) = (mathf::round_i32(fx), mathf::round_i32(fy));
    let within = (point.0 - fx, point.1 - fy);
    let edge = mathf::fmin(
        mathf::fmin(within.0, 1.0 - within.0),
        mathf::fmin(within.1, 1.0 - within.1),
    );

    let mut best = [(f64::MAX, site_of(cx, cy).1); N];
    if !(point.0.is_finite() && point.1.is_finite()) {
        return best;
    }
    let offer = |best: &mut [(f64, T); N], dx: i32, dy: i32| {
        let (site, value) = site_of(cx + dx, cy + dy);
        insert(best, square_distance(site, point), value);
    };
    for dy in -1..=1 {
        for dx in -1..=1 {
            offer(&mut best, dx, dy);
        }
    }
    let mut ring: i32 = 1;
    loop {
        let reach = edge + f64::from(ring) + 0.5 - SITE_JITTER;
        if N == 0 || best[N - 1].0 <= reach * reach {
            return best;
        }
        ring += 1;
        for dy in -ring..=ring {
            for dx in -ring..=ring {
                if dx.abs() == ring || dy.abs() == ring {
                    offer(&mut best, dx, dy);
                }
            }
        }
    }
}

/// Place `(d2, value)` among `best`, kept nearest first, if it is strictly
/// nearer than one of them.
fn insert<T: Copy, const N: usize>(best: &mut [(f64, T); N], d2: f64, value: T) {
    let Some(at) = best.iter().position(|&(held, _)| d2 < held) else {
        return;
    };
    best[at..].rotate_right(1);
    best[at] = (d2, value);
}

/// Squared distance, which orders identically to distance and costs no
/// square root.
#[must_use]
pub fn square_distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (a.0 - b.0, a.1 - b.1);
    dx * dx + dy * dy
}

#[cfg(test)]
mod tests;

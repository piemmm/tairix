//! Grid fitting: a drawing made for a known pixel side has its edges moved onto
//! that side's pixel grid, so a small glyph's lines are as crisp as the text
//! beside it.
//!
//! An edge is a contour's horizontal or vertical run, or its extreme on an
//! axis. Each lands on its own [`Phase`], and every other point moves by
//! interpolation between the edges either side of it, as a font hinter carries
//! a glyph's untouched points: curves stay smooth, and the drawing keeps its
//! order on each axis. Every choice is made about the picture's centre, so a
//! drawing symmetric about it stays symmetric — about a line half a pixel
//! aside where its middle edge cannot lie on the centre and stay crisp, a
//! one-pixel stroke down the middle of an even side.

use alloc::vec::Vec;

use tairix_svg::geom::SubPath;
use tairix_util::mathf::{fabs, floor};

/// Nearer than this, in pixels, two coordinates are one.
const SAME: f64 = 1e-6;

/// Edges nearer than this, in pixels, may share a target; further apart, each
/// keeps a pixel of its own.
const DISTINCT: f64 = 0.5;

/// Where on the pixel grid an edge lands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Phase {
    /// A boundary between pixels: a filled edge, or the centre of a stroke an
    /// even number of pixels wide.
    Boundary,
    /// A pixel's centre: the centre of a stroke an odd number of pixels wide.
    Centre,
}

impl Phase {
    /// Where the centre of a stroke `width` whole pixels wide lands, so the
    /// stroke covers whole pixels.
    pub(crate) fn of_stroke(width: f64) -> Self {
        if width - 2.0 * floor(width / 2.0) >= 1.0 {
            Self::Centre
        } else {
            Self::Boundary
        }
    }

    /// How far this phase's grid points nearest `centre` lie from it: `0.0`
    /// or `0.5`, for a centre on a boundary or a pixel's centre.
    fn shift(self, centre: f64) -> f64 {
        let offset = match self {
            Self::Boundary => 0.0,
            Self::Centre => 0.5,
        };
        offset - centre - floor(offset - centre)
    }

    /// The grid point of this phase nearest `at`, a tie going away from
    /// `centre`.
    ///
    /// Either phase's grid is symmetric about a centre on a boundary or a
    /// pixel's centre, so rounding the distance from it rounds a mirrored pair
    /// alike.
    fn snap(self, at: f64, centre: f64) -> f64 {
        let shift = self.shift(centre);
        let distance = at - centre;
        let snapped = floor(fabs(distance) - shift + 0.5) + shift;
        if distance < 0.0 {
            centre - snapped
        } else {
            centre + snapped
        }
    }
}

/// The fit of one drawing: on each axis, its edges in source order, each with
/// the pixel coordinate it moves to.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct GridFit {
    x: Vec<(f64, f64)>,
    y: Vec<(f64, f64)>,
}

impl GridFit {
    /// The fit of a drawing `side` pixels square made of `parts`: contours in
    /// pixels, each part with the phase its edges land on. Fitting the parts
    /// together keeps one part's edge where it met another's.
    pub(crate) fn new<'a>(
        side: u32,
        parts: impl IntoIterator<Item = (&'a [SubPath], Phase)>,
    ) -> Self {
        let (mut x, mut y) = (Vec::new(), Vec::new());
        for (contours, phase) in parts {
            for contour in contours {
                gather(contour, phase, &mut x, &mut y);
            }
        }
        let centre = f64::from(side) / 2.0;
        Self {
            x: settle(x, centre),
            y: settle(y, centre),
        }
    }

    /// Move every point of `contours` by the fit.
    pub(crate) fn apply(&self, contours: &mut [SubPath]) {
        for point in contours.iter_mut().flat_map(|contour| &mut contour.points) {
            *point = (moved(&self.x, point.0), moved(&self.y, point.1));
        }
    }
}

/// Add `contour`'s edges, landing on `phase`, to the `x` and `y` axes'.
fn gather(contour: &SubPath, phase: Phase, x: &mut Vec<(f64, Phase)>, y: &mut Vec<(f64, Phase)>) {
    let points = &contour.points;
    let (Some(&first), Some(&last)) = (points.first(), points.last()) else {
        return;
    };
    let (mut low, mut high) = (first, first);
    for &(px, py) in points {
        low = (low.0.min(px), low.1.min(py));
        high = (high.0.max(px), high.1.max(py));
    }
    x.extend([(low.0, phase), (high.0, phase)]);
    y.extend([(low.1, phase), (high.1, phase)]);
    let closing = contour.closed.then_some((last, first));
    let runs = points.windows(2).map(|pair| (pair[0], pair[1]));
    for (a, b) in runs.chain(closing) {
        let (across, along) = ((a.0 - b.0).abs(), (a.1 - b.1).abs());
        if across <= SAME && along > SAME {
            x.push((f64::midpoint(a.0, b.0), phase));
        } else if along <= SAME && across > SAME {
            y.push((f64::midpoint(a.1, b.1), phase));
        }
    }
}

/// The axis's edges in order, each with where it lands: on its phase's
/// nearest grid point, but never nearer `centre` than the edge inside it, and a
/// pixel clear of it unless the two were all but one edge already.
///
/// Worked outward from the centre on each side, so a mirrored pair of edges
/// is settled alike. An edge on the centre that its phase's grid misses moves
/// the whole axis half a pixel aside first, so it lands on its grid and the
/// drawing about it stays symmetric.
fn settle(mut edges: Vec<(f64, Phase)>, centre: f64) -> Vec<(f64, f64)> {
    // Stable, so of two edges at one place the first gathered decides.
    edges.sort_by(|a, b| a.0.total_cmp(&b.0));
    edges.dedup_by(|later, earlier| later.0 - earlier.0 <= SAME);
    let off_grid = |&(source, phase): &(f64, Phase)| {
        fabs(source - centre) <= SAME && phase.shift(centre) > 0.0
    };
    let aside = if edges.iter().any(off_grid) { 0.5 } else { 0.0 };
    let mut keys: Vec<(f64, f64)> = edges
        .into_iter()
        .map(|(source, phase)| (source, phase.snap(source + aside, centre + aside)))
        .collect();
    let split = keys.partition_point(|&(source, _)| source < centre);
    let (left, right) = keys.split_at_mut(split);
    if let (Some(inner_left), Some(inner_right)) = (left.last_mut(), right.first_mut()) {
        if inner_right.1 <= inner_left.1 && inner_right.0 - inner_left.0 >= DISTINCT {
            inner_left.1 -= 1.0;
            inner_right.1 += 1.0;
        }
    }
    outward(right.iter_mut(), 1.0);
    outward(left.iter_mut().rev(), -1.0);
    keys
}

/// Hold each key of `keys`, ordered from the centre outward in direction
/// `out` (`1.0` or `-1.0`), clear of the key inside it.
fn outward<'a>(keys: impl Iterator<Item = &'a mut (f64, f64)>, out: f64) {
    let mut inner: Option<(f64, f64)> = None;
    for key in keys {
        if let Some((inner_source, inner_target)) = inner {
            let beyond = (key.1 - inner_target) * out;
            if beyond <= 0.0 {
                key.1 = if fabs(key.0 - inner_source) < DISTINCT {
                    inner_target
                } else {
                    key.1 + out * (floor(-beyond) + 1.0)
                };
            }
        }
        inner = Some(*key);
    }
}

/// Where `at` moves on an axis whose edges move as `keys` say: an edge to its
/// target, a point between two edges in proportion between theirs, and a
/// point beyond the outermost edge with it.
fn moved(keys: &[(f64, f64)], at: f64) -> f64 {
    let (Some(&(first, first_to)), Some(&(last, last_to))) = (keys.first(), keys.last()) else {
        return at;
    };
    if at <= first {
        return at + (first_to - first);
    }
    if at >= last {
        return at + (last_to - last);
    }
    let after = keys.partition_point(|&(source, _)| source <= at);
    match (keys.get(after.wrapping_sub(1)), keys.get(after)) {
        (Some(&(s0, t0)), Some(&(s1, t1))) => t0 + (at - s0) * (t1 - t0) / (s1 - s0),
        _ => at,
    }
}

#[cfg(test)]
#[path = "fit_tests.rs"]
mod tests;

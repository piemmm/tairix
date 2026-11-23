//! Flattening as RISC OS 5's Draw module does it, recorded: a curve halved
//! at its middle, each new point the floor of the mean of two, until each
//! control point lies within the flatness of a third of the way along its
//! chord, measured across both axes at once.

use crate::geometry::Point;

/// The flatness a nought asks for, in user units.
pub(crate) const DEFAULT: i64 = 512;

/// How deep halving may go before RISC OS's own would have run out of
/// stack: no curve within a word needs more than forty halvings while the
/// flatness is positive.
const DEEPEST: usize = 64;

/// The test a curve's control points are held to: four times the flatness,
/// as RISC OS keeps it in a word, nought for the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Threshold(i64);

impl Threshold {
    /// The threshold flatness `flatness` sets.
    #[must_use]
    pub(crate) fn of(flatness: u32) -> Self {
        let held = (flatness << 2).cast_signed();
        Self(if held == 0 {
            4 * DEFAULT
        } else {
            i64::from(held)
        })
    }
}

/// Halving went past [`DEEPEST`]: a negative threshold, which RISC OS
/// follows until its stack overflows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TooDeep;

#[derive(Clone, Copy)]
struct Curve([Point; 4]);

fn mid(a: Point, b: Point) -> Point {
    // Exact, as RISC OS's sums carry into a thirty-third bit.
    let mean = |u: i32, v: i32| {
        let floor = (i64::from(u) + i64::from(v)) >> 1;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the mean of two words lies between them"
        )]
        let held = floor as i32;
        held
    };
    Point::new(mean(a.x, b.x), mean(a.y, b.y))
}

impl Curve {
    fn halves(self) -> (Self, Self) {
        let [p0, p1, p2, p3] = self.0;
        let (q0, q1, q2) = (mid(p0, p1), mid(p1, p2), mid(p2, p3));
        let (r0, r1) = (mid(q0, q1), mid(q1, q2));
        let split = mid(r0, r1);
        (Self([p0, q0, r0, split]), Self([split, r1, q2, p3]))
    }

    fn flat(self, threshold: Threshold) -> bool {
        let [p0, p1, p2, p3] = self.0.map(|p| (i64::from(p.x), i64::from(p.y)));
        let near = |c: (i64, i64), from: (i64, i64), to: (i64, i64)| {
            (3 * c.0 - 2 * from.0 - to.0).abs() + (3 * c.1 - 2 * from.1 - to.1).abs()
        };
        near(p1, p0, p3).max(near(p2, p3, p0)) <= threshold.0
    }
}

/// The ends of the lines `from`'s curve through `c1` and `c2` to `to`
/// flattens to, in order, handed to `line`.
///
/// # Errors
///
/// [`TooDeep`] before any line where halving would not end; what `line`
/// fails with.
pub(crate) fn flatten<E: From<TooDeep>>(
    from: Point,
    c1: Point,
    c2: Point,
    to: Point,
    threshold: Threshold,
    mut line: impl FnMut(Point) -> Result<(), E>,
) -> Result<(), E> {
    if threshold.0 < 0 {
        return Err(TooDeep.into());
    }
    let mut stack = [(Curve([from; 4]), 0usize); DEEPEST + 1];
    stack[0] = (Curve([from, c1, c2, to]), 0);
    let mut held = 1;
    while held > 0 {
        held -= 1;
        let (curve, depth) = stack[held];
        if curve.flat(threshold) {
            line(curve.0[3])?;
            continue;
        }
        if depth >= DEEPEST {
            return Err(TooDeep.into());
        }
        let (first, second) = curve.halves();
        stack[held] = (second, depth + 1);
        stack[held + 1] = (first, depth + 1);
        held += 2;
    }
    Ok(())
}

#[cfg(test)]
mod tests;

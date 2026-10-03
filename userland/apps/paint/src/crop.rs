//! The crop tool's box: the part of the picture kept, set out by a drag and
//! adjusted by an edge, a corner or its middle before it is applied.

use crate::shape::Bounds;

/// Which of a box's two edges along one axis a drag carries.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Side {
    /// Neither.
    #[default]
    Neither,
    /// The left or top.
    Near,
    /// The right or bottom.
    Far,
}

/// What a drag carries of a box: the box whole, or an edge or a corner.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Grab {
    /// The box, moved whole.
    Whole,
    /// The edges taken across and down: one for an edge, two for a corner.
    Edges {
        /// The left or right edge, if either.
        across: Side,
        /// The top or bottom edge, if either.
        down: Side,
    },
}

impl Grab {
    /// What a press at screen point `at` takes of a box shown over the
    /// screen pixels `edge`, each edge answering within `reach` pixels of
    /// its line: the nearer of two edges a narrow box puts within reach, a
    /// corner where two meet, the box whole from inside it, and nothing
    /// outside.
    #[must_use]
    pub fn of(edge: Bounds, (x, y): (i64, i64), reach: i64) -> Option<Self> {
        let inside_reach = x >= edge.x0 - reach
            && x < edge.x1 + reach
            && y >= edge.y0 - reach
            && y < edge.y1 + reach;
        if !inside_reach || edge.is_empty() {
            return None;
        }
        let nearer = |at: i64, low: i64, high: i64| {
            let (to_low, to_high) = ((at - low).abs(), (at - high).abs());
            match (to_low <= reach, to_high <= reach) {
                (true, true) if to_low <= to_high => Side::Near,
                (true, false) => Side::Near,
                (_, true) => Side::Far,
                (false, false) => Side::Neither,
            }
        };
        let across = nearer(x, edge.x0, edge.x1 - 1);
        let down = nearer(y, edge.y0, edge.y1 - 1);
        if (across, down) != (Side::Neither, Side::Neither) {
            return Some(Self::Edges { across, down });
        }
        let inside = (edge.x0..edge.x1).contains(&x) && (edge.y0..edge.y1).contains(&y);
        inside.then_some(Self::Whole)
    }

    /// `start` with what this takes moved `(dx, dy)` pixels, held to
    /// `within`, which holds `start`, and never under a pixel across.
    #[must_use]
    pub fn dragged(self, start: Bounds, (dx, dy): (i64, i64), within: Bounds) -> Bounds {
        let Self::Edges { across, down } = self else {
            let dx = dx.max(within.x0 - start.x0).min(within.x1 - start.x1);
            let dy = dy.max(within.y0 - start.y0).min(within.y1 - start.y1);
            return Bounds {
                x0: start.x0 + dx,
                y0: start.y0 + dy,
                x1: start.x1 + dx,
                y1: start.y1 + dy,
            };
        };
        let mut moved = start;
        match across {
            Side::Near => moved.x0 = (start.x0 + dx).max(within.x0).min(start.x1 - 1),
            Side::Far => moved.x1 = (start.x1 + dx).min(within.x1).max(start.x0 + 1),
            Side::Neither => {}
        }
        match down {
            Side::Near => moved.y0 = (start.y0 + dy).max(within.y0).min(start.y1 - 1),
            Side::Far => moved.y1 = (start.y1 + dy).min(within.y1).max(start.y0 + 1),
            Side::Neither => {}
        }
        moved
    }
}

/// The box a drag from pixel `from` to pixel `to` sets out, both inclusive,
/// held to `within`.
#[must_use]
pub fn set_out(from: (i64, i64), to: (i64, i64), within: Bounds) -> Bounds {
    Bounds {
        x0: from.0.min(to.0),
        y0: from.1.min(to.1),
        x1: from.0.max(to.0) + 1,
        y1: from.1.max(to.1) + 1,
    }
    .intersection(&within)
}

/// The eight handles of a box shown over the screen pixels `edge`, each a
/// square `side` across: its corners and the middles of its edges.
#[must_use]
pub fn handles(edge: Bounds, side: i64) -> [Bounds; 8] {
    let half = side / 2;
    let (mid_x, mid_y) = (
        i64::midpoint(edge.x0, edge.x1),
        i64::midpoint(edge.y0, edge.y1),
    );
    let at = |x: i64, y: i64| Bounds {
        x0: x - half,
        y0: y - half,
        x1: x - half + side,
        y1: y - half + side,
    };
    let (right, bottom) = (edge.x1 - 1, edge.y1 - 1);
    [
        at(edge.x0, edge.y0),
        at(mid_x, edge.y0),
        at(right, edge.y0),
        at(right, mid_y),
        at(right, bottom),
        at(mid_x, bottom),
        at(edge.x0, bottom),
        at(edge.x0, mid_y),
    ]
}

#[cfg(test)]
#[path = "crop_tests.rs"]
mod tests;

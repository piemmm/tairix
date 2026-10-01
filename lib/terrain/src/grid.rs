//! A square grid of samples, and the eight directions between neighbours.

/// A square grid `side` samples a side, stored row-major.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Grid {
    side: u32,
}

impl Grid {
    /// A grid `side` samples a side.
    #[must_use]
    pub const fn new(side: u32) -> Self {
        Self { side }
    }

    /// Samples along each side.
    #[must_use]
    pub const fn side(self) -> u32 {
        self.side
    }

    /// How many samples the grid holds.
    #[must_use]
    pub const fn area(self) -> usize {
        (self.side as usize) * (self.side as usize)
    }

    /// The row-major index of the in-range sample `(x, y)`.
    #[must_use]
    pub const fn index(self, x: u32, y: u32) -> usize {
        (y as usize) * (self.side as usize) + (x as usize)
    }

    /// The sample at row-major `index`, which is below [`area`](Self::area).
    #[must_use]
    pub fn position(self, index: usize) -> (u32, u32) {
        let side = (self.side as usize).max(1);
        let narrow = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
        (narrow(index % side), narrow(index / side))
    }

    /// The index of the sample `(dx, dy)` from `(x, y)`, or `None` off the
    /// grid.
    #[must_use]
    pub fn neighbour(self, x: u32, y: u32, dx: i32, dy: i32) -> Option<usize> {
        let nx = x.checked_add_signed(dx)?;
        let ny = y.checked_add_signed(dy)?;
        (nx < self.side && ny < self.side).then(|| self.index(nx, ny))
    }

    /// Whether `(x, y)` lies on the grid's outer edge.
    #[must_use]
    pub const fn is_rim(self, x: u32, y: u32) -> bool {
        x == 0 || y == 0 || x + 1 == self.side || y + 1 == self.side
    }
}

/// Where a sample drains to.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(u8)]
pub enum FlowDir {
    /// Drains out of the grid: an outlet, or the one sample of a closed
    /// basin its flood started from.
    #[default]
    Sink = 0,
    /// East.
    E = 1,
    /// South-east.
    Se = 2,
    /// South.
    S = 3,
    /// South-west.
    Sw = 4,
    /// West.
    W = 5,
    /// North-west.
    Nw = 6,
    /// North.
    N = 7,
    /// North-east.
    Ne = 8,
}

impl FlowDir {
    /// The grid offset this direction moves by, or `None` for a sink.
    #[must_use]
    pub const fn offset(self) -> Option<(i32, i32)> {
        match self {
            Self::Sink => None,
            Self::E => Some((1, 0)),
            Self::Se => Some((1, 1)),
            Self::S => Some((0, 1)),
            Self::Sw => Some((-1, 1)),
            Self::W => Some((-1, 0)),
            Self::Nw => Some((-1, -1)),
            Self::N => Some((0, -1)),
            Self::Ne => Some((1, -1)),
        }
    }

    /// How far a step this way runs, in samples: one straight, `√2`
    /// diagonally, nought for a sink.
    #[must_use]
    pub const fn length(self) -> f64 {
        match self {
            Self::Sink => 0.0,
            Self::E | Self::S | Self::W | Self::N => 1.0,
            Self::Se | Self::Sw | Self::Nw | Self::Ne => core::f64::consts::SQRT_2,
        }
    }
}

/// The eight neighbours, each with its offset and length, in the fixed order
/// every tie is broken by: the direction enum's, so a tie resolves to the
/// lowest-numbered direction whatever the grid, the seed or the traversal.
pub const NEIGHBOURS: [(FlowDir, i32, i32, f64); 8] = [
    (FlowDir::E, 1, 0, 1.0),
    (FlowDir::Se, 1, 1, core::f64::consts::SQRT_2),
    (FlowDir::S, 0, 1, 1.0),
    (FlowDir::Sw, -1, 1, core::f64::consts::SQRT_2),
    (FlowDir::W, -1, 0, 1.0),
    (FlowDir::Nw, -1, -1, core::f64::consts::SQRT_2),
    (FlowDir::N, 0, -1, 1.0),
    (FlowDir::Ne, 1, -1, core::f64::consts::SQRT_2),
];

#[cfg(test)]
#[path = "grid_tests.rs"]
mod tests;

//! A land for the tests to lay countryside over: rolling hills falling to a
//! river that winds across them, stony on the heights, wet by the water,
//! wooded in patches.

use tairix_util::mathf;

use crate::ground::{Ground, Lie, Waters};
use crate::plane::Point;

/// Rolling hills and their river.
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct Hills;

/// How broad the river runs, and how deep it stands over its bed.
const RIVER: f64 = 7.0;
const DEPTH: f64 = 1.2;

impl Hills {
    /// How far `at` lies across the river from its middle line.
    fn from_river(at: Point) -> f64 {
        let middle = 260.0 + 420.0 * mathf::sin(at.y / 1700.0) + 90.0 * mathf::sin(at.y / 430.0);
        (at.x - middle).abs()
    }

    /// The ground's height before the river's bed is cut.
    fn rolling(at: Point) -> f64 {
        35.0 * mathf::sin(at.x / 820.0) * mathf::cos(at.y / 960.0)
            + 18.0 * mathf::sin((at.x + 0.6 * at.y) / 390.0)
            + 6.0 * mathf::cos((at.x - at.y) / 150.0)
            + 60.0
    }

    /// The level the river's water stands at beside `at`.
    fn river_level(at: Point) -> f64 {
        20.0 + 0.004 * at.y
    }
}

impl Waters for Hills {
    fn height(&self, at: Point) -> f64 {
        let across = Self::from_river(at);
        let level = Self::river_level(at);
        let valley = (across / 600.0).min(1.0);
        let bank = level + 1.5 + (Self::rolling(at) - level - 1.5) * valley * valley;
        if across < RIVER {
            level - DEPTH
        } else {
            bank.max(level + 0.3)
        }
    }

    fn water(&self, at: Point) -> Option<f64> {
        (Self::from_river(at) < RIVER).then(|| Self::river_level(at))
    }
}

impl Ground for Hills {
    fn lie(&self, at: Point) -> Lie {
        let height = self.height(at);
        let across = Self::from_river(at);
        Lie {
            wet: (1.0 - across / 140.0).clamp(0.0, 1.0),
            stony: ((height - 70.0) / 35.0).clamp(0.0, 1.0),
            wooded: (0.5 + 0.5 * mathf::sin(at.x / 610.0 + at.y / 770.0)).clamp(0.0, 1.0),
            fertile: (1.0 - (height - 30.0) / 70.0).clamp(0.0, 1.0),
        }
    }
}

/// Level, dry ground everywhere: for a test whose subject is not the land.
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct Flat;

impl Waters for Flat {
    fn height(&self, _: Point) -> f64 {
        10.0
    }

    fn water(&self, _: Point) -> Option<f64> {
        None
    }
}

impl Ground for Flat {
    fn lie(&self, _: Point) -> Lie {
        Lie {
            wet: 0.0,
            stony: 0.2,
            wooded: 0.3,
            fertile: 0.8,
        }
    }
}

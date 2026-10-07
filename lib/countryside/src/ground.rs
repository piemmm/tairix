//! The land a countryside is laid out over, as its consumer knows it.

use tairix_util::mathf;

use crate::plane::Point;

/// The land's surface beneath a layout, and the water standing on it: all
/// that asking what lies at a place reads. A pure function of the place
/// asked about, so a layout asks the same questions of it however it is
/// laid out.
pub trait Waters: Sync {
    /// The ground's height at `at`, in metres.
    fn height(&self, at: Point) -> f64;

    /// The surface of the water standing at `at`, where water stands over the
    /// ground there: the sea, a lake, a river or a pond.
    fn water(&self, at: Point) -> Option<f64>;
}

/// The land beneath a layout as laying it out reads it: its surface and
/// water, and what its ground is like.
pub trait Ground: Waters {
    /// What the ground is like at `at`.
    fn lie(&self, at: Point) -> Lie;
}

/// What the ground is like at a place, each in `0.0..=1.0`.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Lie {
    /// How wet it lies: nought where it drains, one where it is a marsh.
    pub wet: f64,
    /// How much stone lies at hand in it: one where rock comes to the surface.
    pub stony: f64,
    /// How wooded the country about it is.
    pub wooded: f64,
    /// How deep and good its soil is.
    pub fertile: f64,
}

/// How steeply `ground` rises at `at`, as rise over run, and the way it rises,
/// read over `step` either way.
pub(crate) fn slope(ground: &dyn Waters, at: Point, step: f64) -> (f64, Point) {
    let dx =
        ground.height(Point::new(at.x + step, at.y)) - ground.height(Point::new(at.x - step, at.y));
    let dy =
        ground.height(Point::new(at.x, at.y + step)) - ground.height(Point::new(at.x, at.y - step));
    let rise = Point::new(dx, dy) * (0.5 / step);
    (mathf::hypot(rise.x, rise.y), rise.normalized())
}

/// Whether water stands over the ground at `at`.
pub(crate) fn wet<W: Waters + ?Sized>(ground: &W, at: Point) -> bool {
    ground
        .water(at)
        .is_some_and(|level| level > ground.height(at))
}

//! How the canvas looks onto the drawing: the zoom, and where the drawing's
//! origin falls in the canvas.
//!
//! The drawing is seen in device pixels: a drawing pixel at 100% is one
//! logical pixel, so `zoom × scale` device pixels. The pan is held in whole
//! device pixels, so the tiles the drawing is rendered in land on the pixel
//! grid wherever the drawing is scrolled.

use alloc::string::String;

use tairix_bezier::{Affine, Bounds, Point};
use tairix_util::mathf::{fmax, fmin, round, round_i32};

/// The least zoom: 1%.
pub const LEAST_ZOOM: f64 = 0.01;

/// The farthest from the drawing's origin, in drawing pixels, the canvas is
/// taken: twice the largest coordinate a drawing holds, so whatever a
/// drawing's transforms make of its bounds, the view stays where whole
/// device pixels and tile indices are exact.
pub const REACH: f64 = 2.0 * tairix_drawing::limits::MAX_COORDINATE;

/// `at`, a drawing coordinate, held within [`REACH`].
#[must_use]
pub fn within_reach(at: f64) -> f64 {
    if at.is_nan() {
        0.0
    } else {
        at.clamp(-REACH, REACH)
    }
}

/// The greatest zoom: 25600%.
pub const MOST_ZOOM: f64 = 256.0;

/// `zoom` as the status band and the zoom menu read it: a whole percentage,
/// or to one place below 10%.
#[must_use]
pub fn readout(zoom: f64) -> String {
    let percent = zoom * 100.0;
    if percent < 10.0 {
        alloc::format!("{percent:.1}%")
    } else {
        alloc::format!("{}%", round_i32(percent))
    }
}

/// The zooms Zoom in and Zoom out step through.
pub const STEPS: [f64; 26] = [
    0.01,
    0.02,
    0.03,
    0.05,
    0.08,
    0.125,
    1.0 / 6.0,
    0.25,
    1.0 / 3.0,
    0.5,
    2.0 / 3.0,
    1.0,
    1.5,
    2.0,
    3.0,
    4.0,
    6.0,
    8.0,
    12.0,
    16.0,
    24.0,
    32.0,
    48.0,
    64.0,
    128.0,
    256.0,
];

/// The canvas's view of the drawing.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Viewport {
    zoom: f64,
    /// Device pixels per logical pixel.
    scale: f64,
    /// Where the drawing's origin falls, in device pixels from the canvas's
    /// top-left corner; always whole.
    origin: Point,
}

impl Viewport {
    /// A view at `zoom` for a display of `scale` device pixels to the logical
    /// pixel, the drawing's origin at the canvas's corner.
    #[must_use]
    pub fn new(zoom: f64, scale: f64) -> Self {
        Self {
            zoom: zoom.clamp(LEAST_ZOOM, MOST_ZOOM),
            scale,
            origin: Point::ZERO,
        }
    }

    /// The zoom, 1.0 at 100%.
    #[must_use]
    pub const fn zoom(&self) -> f64 {
        self.zoom
    }

    /// Device pixels per drawing pixel.
    #[must_use]
    pub fn pixels(&self) -> f64 {
        self.zoom * self.scale
    }

    /// Where the drawing's origin falls in the canvas, in device pixels.
    #[must_use]
    pub const fn origin(&self) -> Point {
        self.origin
    }

    /// The map from the drawing onto the device grid its tiles are rendered
    /// on: the drawing's origin at the grid's.
    #[must_use]
    pub fn to_tiles(&self) -> Affine {
        Affine::scale(self.pixels(), self.pixels())
    }

    /// The map from the drawing onto the canvas, in device pixels.
    #[must_use]
    pub fn to_canvas(&self) -> Affine {
        self.to_tiles()
            .then(Affine::translate(self.origin.x, self.origin.y))
    }

    /// The drawing point under the canvas's device point `at`.
    #[must_use]
    pub fn drawing_point(&self, at: Point) -> Point {
        (at - self.origin) / self.pixels()
    }

    /// The canvas's device point over the drawing point `point`.
    #[must_use]
    pub fn canvas_point(&self, point: Point) -> Point {
        point * self.pixels() + self.origin
    }

    /// The display's scale changed to `scale`, the drawing point at the
    /// canvas's centre `centre` kept there.
    pub fn rescale(&mut self, scale: f64, centre: Point) {
        let held = self.drawing_point(centre);
        self.scale = scale;
        self.place(held, centre);
    }

    /// Scroll by `by` device pixels, no farther than [`REACH`].
    pub fn scroll(&mut self, by: Point) {
        self.origin = self.held(self.origin - by);
    }

    /// Zoom to `zoom`, keeping the drawing point under the canvas's device
    /// point `anchor` where it is.
    pub fn zoom_at(&mut self, zoom: f64, anchor: Point) {
        let held = self.drawing_point(anchor);
        self.zoom = zoom.clamp(LEAST_ZOOM, MOST_ZOOM);
        self.place(held, anchor);
    }

    /// The next zoom step in from the present zoom, or out when `out`.
    #[must_use]
    pub fn stepped(&self, out: bool) -> f64 {
        // A zoom within a hair of a step is that step.
        let slack = 1.0 + 1e-9;
        if out {
            STEPS
                .iter()
                .rev()
                .copied()
                .find(|&step| step * slack < self.zoom)
                .unwrap_or(LEAST_ZOOM)
        } else {
            STEPS
                .iter()
                .copied()
                .find(|&step| step > self.zoom * slack)
                .unwrap_or(MOST_ZOOM)
        }
    }

    /// Zoom and scroll so `bounds` fills a canvas of `size` device pixels,
    /// centred, with `margin` device pixels about it.
    pub fn fit(&mut self, bounds: Bounds, size: Point, margin: f64) {
        if bounds.is_empty() || !bounds.is_finite() {
            return;
        }
        let room = Point::new(
            fmax(size.x - 2.0 * margin, 1.0),
            fmax(size.y - 2.0 * margin, 1.0),
        );
        let across = fmax(bounds.width(), f64::MIN_POSITIVE);
        let down = fmax(bounds.height(), f64::MIN_POSITIVE);
        let pixels = fmin(room.x / across, room.y / down);
        self.zoom = (pixels / self.scale).clamp(LEAST_ZOOM, MOST_ZOOM);
        self.place(bounds.centre(), size / 2.0);
    }

    /// Put the drawing point `point` at the canvas's device point `at`.
    fn place(&mut self, point: Point, at: Point) {
        let point = Point::new(within_reach(point.x), within_reach(point.y));
        self.origin = self.held(at - point * self.pixels());
    }

    /// `origin` made whole and held where the canvas's corner shows a point
    /// within [`REACH`].
    fn held(&self, origin: Point) -> Point {
        let most = round(REACH * self.pixels());
        let hold = |at: f64| {
            if at.is_nan() {
                0.0
            } else {
                at.clamp(-most, most)
            }
        };
        whole(Point::new(hold(origin.x), hold(origin.y)))
    }
}

fn whole(point: Point) -> Point {
    Point::new(round(point.x), round(point.y))
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "the values compared are set and read exactly"
)]
mod tests {
    use tairix_bezier::{Bounds, Point};

    use super::{Viewport, LEAST_ZOOM, MOST_ZOOM, REACH};

    #[test]
    fn zooming_keeps_the_point_under_the_anchor() {
        let mut view = Viewport::new(1.0, 2.0);
        let anchor = Point::new(300.0, 200.0);
        let under = view.drawing_point(anchor);
        view.zoom_at(4.0, anchor);
        assert!(view.drawing_point(anchor).distance(under) < 1.0 / view.pixels());
        assert_eq!(
            view.origin().x,
            view.origin().x.round(),
            "panned in whole pixels"
        );
    }

    #[test]
    fn zoom_steps_climb_and_fall_through_the_table_and_stop_at_its_ends() {
        let view = Viewport::new(1.0, 1.0);
        assert_eq!(view.stepped(false), 1.5);
        assert_eq!(view.stepped(true), 2.0 / 3.0);
        assert_eq!(Viewport::new(MOST_ZOOM, 1.0).stepped(false), MOST_ZOOM);
        assert_eq!(Viewport::new(LEAST_ZOOM, 1.0).stepped(true), LEAST_ZOOM);
        assert_eq!(
            Viewport::new(0.7, 1.0).stepped(true),
            2.0 / 3.0,
            "a zoom between steps"
        );
        assert_eq!(
            Viewport::new(1e6, 1.0).zoom(),
            MOST_ZOOM,
            "held to its range"
        );
    }

    #[test]
    fn the_view_is_never_taken_past_its_reach() {
        let mut view = Viewport::new(MOST_ZOOM, 2.0);
        view.scroll(Point::new(f64::MAX, -f64::INFINITY));
        let corner = view.drawing_point(Point::ZERO);
        assert!(corner.x.abs() <= REACH * 1.0001 && corner.y.abs() <= REACH * 1.0001);
        view.fit(
            Bounds::new(Point::new(1e300, 1e300), Point::new(2e300, 2e300)),
            Point::new(800.0, 600.0),
            0.0,
        );
        let centre = view.drawing_point(Point::new(400.0, 300.0));
        assert!(centre.x <= REACH * 1.0001, "{centre:?}");
        view.scroll(Point::new(f64::NAN, 0.0));
        assert!(view.origin().x.is_finite());
    }

    #[test]
    fn fitting_a_page_centres_it_whole_in_the_canvas() {
        let mut view = Viewport::new(1.0, 1.0);
        let page = Bounds::new(Point::ZERO, Point::new(800.0, 400.0));
        view.fit(page, Point::new(420.0, 420.0), 10.0);
        assert!((view.zoom() - 0.5).abs() < 1e-12);
        let centre = view.canvas_point(page.centre());
        assert!(centre.distance(Point::new(210.0, 210.0)) <= 1.0);
    }
}

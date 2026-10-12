//! The graph-space viewport: the pan offset and zoom the GUI drives, and the
//! window↔graph mapping built on them.
//!
//! Zoom is an integer per-mille factor; the pan is the graph-space point shown
//! at the window's centre. Zoom about an anchor keeps the graph point under the
//! pointer fixed, which is the contract a pinch and a Ctrl+wheel both need. The
//! arithmetic mirrors `userland/apps/view`'s image viewport, but over graph
//! coordinates rather than image pixels, so it is a sibling, not a shared type.

use tairix_geometry::Point;

use crate::model::Map;

/// Smallest zoom, 0.1×.
pub const MIN_ZOOM_PER_MILLE: u32 = 100;
/// Largest zoom, 8×.
pub const MAX_ZOOM_PER_MILLE: u32 = 8_000;
/// The zoom a fresh view opens at, 1×.
pub const DEFAULT_ZOOM_PER_MILLE: u32 = 1_000;

/// Graph-space margin kept reachable around the laid-out extent when clamping
/// the pan, so the map can never be dragged entirely off-screen.
const PAN_MARGIN: i32 = 400;

/// The pan/zoom state mapping graph space to a window.
#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    zoom: u32,
    center: Point,
    view_w: u32,
    view_h: u32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            zoom: DEFAULT_ZOOM_PER_MILLE,
            center: Point::ORIGIN,
            view_w: 0,
            view_h: 0,
        }
    }
}

impl Viewport {
    /// A viewport over a window of `view_w` × `view_h`, centred on the origin
    /// at 1×.
    #[must_use]
    pub fn new(view_w: u32, view_h: u32) -> Self {
        Self {
            view_w,
            view_h,
            ..Self::default()
        }
    }

    /// The current zoom, in per-mille.
    #[must_use]
    pub fn zoom(&self) -> u32 {
        self.zoom
    }

    /// The graph-space point shown at the window centre.
    #[must_use]
    pub fn center(&self) -> Point {
        self.center
    }

    /// Resize the window the viewport maps onto.
    pub fn set_view_size(&mut self, view_w: u32, view_h: u32) {
        self.view_w = view_w;
        self.view_h = view_h;
    }

    fn view_center(&self) -> (i64, i64) {
        (i64::from(self.view_w / 2), i64::from(self.view_h / 2))
    }

    /// The window point a graph point maps to at the current pan and zoom.
    #[must_use]
    pub fn to_window(&self, graph: Point) -> Point {
        let (vcx, vcy) = self.view_center();
        let zoom = i64::from(self.zoom);
        let dx = i64::from(graph.x - self.center.x) * zoom / 1000;
        let dy = i64::from(graph.y - self.center.y) * zoom / 1000;
        Point::new(saturating_i32(vcx + dx), saturating_i32(vcy + dy))
    }

    /// The graph point a window point maps back to at the current pan and zoom.
    #[must_use]
    pub fn to_graph(&self, window: Point) -> Point {
        let (vcx, vcy) = self.view_center();
        let zoom = i64::from(self.zoom);
        let dx = i64::from(window.x) - vcx;
        let dy = i64::from(window.y) - vcy;
        Point::new(
            saturating_i32(i64::from(self.center.x) + dx * 1000 / zoom),
            saturating_i32(i64::from(self.center.y) + dy * 1000 / zoom),
        )
    }

    /// Set the zoom to `new_zoom` (clamped) while holding the graph point under
    /// `anchor` fixed on screen — the pinch / Ctrl+wheel contract.
    pub fn set_zoom_about(&mut self, anchor: Point, new_zoom: u32) {
        let graph = self.to_graph(anchor);
        self.zoom = new_zoom.clamp(MIN_ZOOM_PER_MILLE, MAX_ZOOM_PER_MILLE);
        let (vcx, vcy) = self.view_center();
        let zoom = i64::from(self.zoom);
        let ax = i64::from(anchor.x) - vcx;
        let ay = i64::from(anchor.y) - vcy;
        self.center = Point::new(
            saturating_i32(i64::from(graph.x) - ax * 1000 / zoom),
            saturating_i32(i64::from(graph.y) - ay * 1000 / zoom),
        );
    }

    /// Pan by a window-space delta: dragging the pointer right by `dx` moves the
    /// content right, i.e. the centred graph point left.
    pub fn pan_by(&mut self, dx: i32, dy: i32) {
        let zoom = i64::from(self.zoom);
        let gdx = i64::from(dx) * 1000 / zoom;
        let gdy = i64::from(dy) * 1000 / zoom;
        self.center = Point::new(
            saturating_i32(i64::from(self.center.x) - gdx),
            saturating_i32(i64::from(self.center.y) - gdy),
        );
    }

    /// Clamp the pan so the laid-out extent stays reachable, keeping the centre
    /// within the map's bounding box expanded by a fixed margin. A map with no
    /// positioned nodes leaves the pan unchanged.
    pub fn clamp_pan(&mut self, map: &Map) {
        let Some((min, max)) = graph_bounds(map) else {
            return;
        };
        let cx = self.center.x.clamp(
            min.x.saturating_sub(PAN_MARGIN),
            max.x.saturating_add(PAN_MARGIN),
        );
        let cy = self.center.y.clamp(
            min.y.saturating_sub(PAN_MARGIN),
            max.y.saturating_add(PAN_MARGIN),
        );
        self.center = Point::new(cx, cy);
    }

    /// Zoom and pan so the whole laid-out map shows with `margin` window
    /// pixels clear on every side, never beyond 1×: a small map is shown at its
    /// own size, not magnified. A map with no nodes leaves the view as it is.
    pub fn fit(&mut self, map: &Map, margin: u32) {
        let Some((min, max)) = graph_bounds(map) else {
            return;
        };
        let span = |lo: i32, hi: i32| u64::from(lo.abs_diff(hi)).max(1);
        let room = |view: u32| u64::from(view.saturating_sub(margin.saturating_mul(2))).max(1);
        let zoom = (room(self.view_w) * 1000 / span(min.x, max.x))
            .min(room(self.view_h) * 1000 / span(min.y, max.y));
        self.zoom = u32::try_from(zoom)
            .unwrap_or(u32::MAX)
            .clamp(MIN_ZOOM_PER_MILLE, DEFAULT_ZOOM_PER_MILLE);
        self.center = Point::new(min.x.midpoint(max.x), min.y.midpoint(max.y));
    }
}

/// The inclusive bounding box of a map's node positions, or `None` if empty.
fn graph_bounds(map: &Map) -> Option<(Point, Point)> {
    let mut nodes = map.nodes().iter();
    let first = nodes.next()?;
    let (mut min, mut max) = (first.pos, first.pos);
    for node in nodes {
        min = Point::new(min.x.min(node.pos.x), min.y.min(node.pos.y));
        max = Point::new(max.x.max(node.pos.x), max.y.max(node.pos.y));
    }
    Some((min, max))
}

/// Clamp an `i64` into `i32` range rather than wrapping.
#[allow(
    clippy::cast_possible_truncation,
    reason = "clamped into i32 range immediately before the cast, so it is exact"
)]
fn saturating_i32(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

#[cfg(test)]
#[path = "viewport_tests.rs"]
mod tests;

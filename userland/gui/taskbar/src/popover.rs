//! Geometry every popover the bar opens shares: how much room it has beside
//! the bar, where it opens from its invoker, and what the shared panel chrome
//! costs it — defined once so the launcher, the notification popover, the
//! readouts, and the volume panel cannot open differently.

use tairix_controls::Panel;
use tairix_geometry::{to_i32, Point, Rect, Scale};
use tairix_theme::Theme;

use crate::edge::Edge;

/// The panel chrome's measured overhead for one popover width: what the
/// header band and frame consume, and where the content region sits.
pub(crate) struct ChromeProbe {
    /// Total vertical pixels the chrome consumes (header plus frame).
    pub(crate) overhead: u32,
    /// The content region's offset from the panel's top edge.
    pub(crate) content_top: i32,
    /// The content region's offset from the panel's left edge.
    pub(crate) content_left: i32,
    /// The content region's width.
    pub(crate) content_width: u32,
}

/// Measure `panel`'s chrome overhead for a popover `width` px wide.
///
/// Probed from the shared [`Panel`] geometry rather than re-derived, so a
/// metrics change can never drift a popover's layout from what the panel
/// actually draws.
pub(crate) fn probe_chrome(panel: &Panel, width: u32, scale: Scale, theme: &Theme) -> ChromeProbe {
    let probe_height = 4096;
    let probe = panel.content_rect(Rect::new(0, 0, width, probe_height), scale, theme);
    match probe {
        Some(content) => ChromeProbe {
            overhead: probe_height.saturating_sub(content.height),
            content_top: content.top(),
            content_left: content.left(),
            content_width: content.width,
        },
        None => ChromeProbe {
            overhead: 0,
            content_top: 0,
            content_left: 0,
            content_width: width,
        },
    }
}

/// The panel origin for a popover of `width` × `height` opened outward from
/// `bar` (pinned to `edge`) and aligned to the `anchor` rectangle it invokes
/// from.
///
/// Along the bar's length the popover is clamped to the *bar*, not to the
/// screen: the bar floats clear of the screen edges it faces, and a popover
/// running on to a screen edge would fill the wallpaper gap the bar leaves.
/// A popover longer than the bar starts at the bar's leading end rather than
/// hanging off its trailing one.
pub(crate) fn panel_origin(edge: Edge, bar: Rect, anchor: Rect, width: u32, height: u32) -> Point {
    let along = |value: i32, start: i32, end: i32, extent: u32| -> i32 {
        let last = end.saturating_sub(to_i32(extent));
        value.clamp(start, last.max(start))
    };
    match edge {
        Edge::Bottom => Point::new(
            along(anchor.left(), bar.left(), bar.right(), width),
            bar.top().saturating_sub(to_i32(height)).max(0),
        ),
        Edge::Top => Point::new(
            along(anchor.left(), bar.left(), bar.right(), width),
            bar.bottom(),
        ),
        Edge::Left => Point::new(
            bar.right(),
            along(anchor.top(), bar.top(), bar.bottom(), height),
        ),
        Edge::Right => Point::new(
            bar.left().saturating_sub(to_i32(width)).max(0),
            along(anchor.top(), bar.top(), bar.bottom(), height),
        ),
    }
}

/// The `(width, height)` a popover has on its side of `bar` (pinned to
/// `edge`): between the bar and the screen edge across from it, so a popover
/// sized to fit never runs back over the bar.
pub(crate) fn room_beside(
    edge: Edge,
    bar: Rect,
    (screen_width, screen_height): (u32, u32),
) -> (u32, u32) {
    let at = |coordinate: i32| u32::try_from(coordinate).unwrap_or(0);
    match edge {
        Edge::Bottom => (screen_width, at(bar.top()).min(screen_height)),
        Edge::Top => (screen_width, screen_height.saturating_sub(at(bar.bottom()))),
        Edge::Left => (screen_width.saturating_sub(at(bar.right())), screen_height),
        Edge::Right => (at(bar.left()).min(screen_width), screen_height),
    }
}

/// The unit step pointing away from a bar pinned to `edge`: the direction a
/// popover it opens rises in.
pub(crate) const fn outward(edge: Edge) -> (i32, i32) {
    match edge {
        Edge::Bottom => (0, -1),
        Edge::Top => (0, 1),
        Edge::Left => (1, 0),
        Edge::Right => (-1, 0),
    }
}

/// Saturating `usize` → `u32`.
pub(crate) fn to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

//! The bar a gradient's stops are edited on: the gradient drawn along a strip
//! over a checker, and beneath it a handle for each stop, coloured as the
//! stop is, the one in hand marked.

use tairix_colour::Rgba;
use tairix_controls::{blend_area, Checker};
use tairix_drawing::Stop;
use tairix_geometry::{Point, Rect, Scale};
use tairix_raster::{Color, Surface};
use tairix_theme::Theme;
use tairix_util::mathf::{fabs, round_i32};

use crate::gradient::ramp;
use crate::layout::px;
use crate::paint::outline_rect;

/// A bar laid out in its bounds: the strip along its top and the band of
/// handles beneath, the strip inset by half a handle at either end so the end
/// stops' handles show whole.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Bar {
    strip: Rect,
    handles: Rect,
    side: u32,
}

impl Bar {
    /// The bar laid out in `bounds`.
    #[must_use]
    pub fn of(bounds: Rect) -> Self {
        let mut rest = bounds;
        let side = (bounds.height * 2 / 5).max(1);
        let handles = rest.take_bottom(side.min(rest.height));
        let inset = side / 2;
        let strip = Rect::new(
            rest.left().saturating_add(px(inset)),
            rest.top(),
            rest.width.saturating_sub(2 * inset).max(1),
            rest.height.saturating_sub(2).max(1),
        );
        Self {
            strip,
            handles,
            side,
        }
    }

    /// Where along the gradient the window's `x` falls, `0..=1`.
    #[must_use]
    pub fn offset_at(&self, x: i32) -> f64 {
        let along = f64::from(x.saturating_sub(self.strip.left()));
        (along / f64::from(self.strip.width)).clamp(0.0, 1.0)
    }

    /// The window `x` a stop at `offset` stands at.
    fn x_of(&self, offset: f64) -> i32 {
        self.strip
            .left()
            .saturating_add(round_i32(offset * f64::from(self.strip.width)))
    }

    /// The handle of `stops` at `point`: `chosen`'s where it is within reach,
    /// else the nearest that is.
    #[must_use]
    pub fn handle_at(&self, stops: &[Stop], chosen: usize, point: Point) -> Option<usize> {
        if !self.handles.contains(point) {
            return None;
        }
        let reach = f64::from(self.side / 2 + 2);
        let distance = |stop: &Stop| fabs(f64::from(point.x - self.x_of(stop.offset)));
        if stops
            .get(chosen)
            .is_some_and(|stop| distance(stop) <= reach)
        {
            return Some(chosen);
        }
        stops
            .iter()
            .enumerate()
            .filter(|(_, stop)| distance(stop) <= reach)
            .min_by(|(_, a), (_, b)| distance(a).total_cmp(&distance(b)))
            .map(|(index, _)| index)
    }

    /// Whether `point` falls on the strip, where a press adds a stop.
    #[must_use]
    pub fn on_strip(&self, point: Point) -> bool {
        self.strip.contains(point)
    }

    /// Paint the bar for `stops`, the one at `chosen` marked, in the accent
    /// where the bar holds the keyboard.
    pub fn render(
        &self,
        surface: &mut Surface,
        (stops, chosen, focused): (&[Stop], usize, bool),
        scale: Scale,
        theme: &Theme,
    ) {
        let palette = theme.palette();
        let checker = Checker::new(theme, scale);
        let checkered = |surface: &mut Surface, rect: Rect| {
            if let Some((x, y)) = rect.surface_origin() {
                checker.paint(surface, x, y, rect.width, rect.height);
            }
        };
        checkered(surface, self.strip);
        if let Some(ramp) = ramp(stops) {
            let width = self.strip.width;
            for column in 0..width {
                let at = (f64::from(column) + 0.5) / f64::from(width);
                let Color { r, g, b, a } = ramp.sample((at, 0.0));
                let slice = Rect::new(
                    self.strip.left().saturating_add(px(column)),
                    self.strip.top(),
                    1,
                    self.strip.height,
                );
                blend_area(surface, slice, Rgba::new(r, g, b, a));
            }
        }
        outline_rect(surface, self.strip, Color::from(palette.border), 1);
        for (index, stop) in stops.iter().enumerate() {
            let rect = Rect::new(
                self.x_of(stop.offset).saturating_sub(px(self.side / 2)),
                self.handles.top(),
                self.side,
                self.side,
            );
            checkered(surface, rect);
            blend_area(surface, rect, stop.colour);
            let (ink, weight) = match (index == chosen, focused) {
                (true, true) => (palette.accent, 2),
                (true, false) => (palette.on_surface, 2),
                (false, _) => (palette.border, 1),
            };
            outline_rect(surface, rect, Color::from(ink), weight);
        }
    }
}

#[cfg(test)]
#[path = "stops_tests.rs"]
mod tests;

//! The curve editor: a tone curve drawn over its channel's histogram, its
//! points added by a press, dragged, nudged by the arrow keys, and taken away
//! by Delete or by being dragged out of the graph.
//!
//! The editor holds which point is chosen and the drag under way; the curve
//! is its owner's, handed in with every event and answered as it now stands.

use tairix_controls::{blend_area, fill_area, Keystroke};
use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_raster::{Color, Surface, SUBPIXEL};
use tairix_theme::Theme;

use crate::histogram::{Histogram, Plot};
use crate::tone::Curve;

/// A point's side, in logical pixels.
const POINT: u32 = 7;

/// How far a press may land from a point and still take it, in logical
/// pixels.
const REACH: u32 = 6;

/// How far past the graph a dragged point must be carried to be taken away,
/// in logical pixels.
const OUT: u32 = 20;

/// How strongly the histogram behind the curve is drawn, in 255ths.
const HISTOGRAM_ALPHA: u8 = 70;

/// What an input to the editor came to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CurveEdit {
    /// The curve as it now stands; `settled` once the interaction is over.
    Changed {
        /// The curve.
        curve: Curve,
        /// Whether the interaction is over.
        settled: bool,
    },
    /// Another point was chosen, the curve unchanged.
    Chose,
}

/// A point being dragged.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Drag {
    index: usize,
    /// Whether it has been carried out of the graph, to be taken away when
    /// let go.
    outside: bool,
}

/// The curve editor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CurveGraph {
    selected: Option<usize>,
    drag: Option<Drag>,
    focused: bool,
    enabled: bool,
    pointer: Point,
}

impl Default for CurveGraph {
    fn default() -> Self {
        Self {
            selected: None,
            drag: None,
            focused: false,
            enabled: true,
            pointer: Point::ORIGIN,
        }
    }
}

impl CurveGraph {
    /// The chosen point.
    #[must_use]
    pub const fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Choose point `index`, or none, as a new curve is shown.
    pub fn select(&mut self, index: Option<usize>) {
        self.selected = index;
        self.drag = None;
    }

    /// Give the editor the keyboard, or take it.
    pub fn focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// Whether it has the keyboard.
    #[must_use]
    pub const fn focused(&self) -> bool {
        self.focused
    }

    /// Offer the editor, or withhold it.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.drag = None;
        }
    }

    /// Whether a point is being dragged.
    #[must_use]
    pub const fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The square the curve is plotted in, inset so a point at an edge is
    /// whole.
    fn plot(bounds: Rect, scale: Scale) -> Rect {
        bounds.inset(scale.scale_length(POINT / 2 + 1))
    }

    fn screen_of(plot: Rect, (input, output): (u8, u8)) -> Point {
        let span = |extent: u32| i64::from(extent.saturating_sub(1));
        let x = (i64::from(input) * span(plot.width) + 127) / 255;
        let y = (i64::from(output) * span(plot.height) + 127) / 255;
        Point::new(
            plot.left() + i32::try_from(x).unwrap_or(0),
            plot.bottom() - 1 - i32::try_from(y).unwrap_or(0),
        )
    }

    fn level_of(at: Point, plot: Rect) -> (u8, u8) {
        let level = |along: i32, extent: u32| {
            let span = i64::from(extent.saturating_sub(1)).max(1);
            let along = i64::from(along).clamp(0, span);
            u8::try_from((along * 255 + span / 2) / span).unwrap_or(u8::MAX)
        };
        (
            level(at.x - plot.left(), plot.width),
            level(plot.bottom() - 1 - at.y, plot.height),
        )
    }

    /// The point of `curve` within reach of `at`, nearest first.
    fn point_at(curve: &Curve, at: Point, plot: Rect, scale: Scale) -> Option<usize> {
        let reach = to_i32(scale.scale_length(REACH));
        curve
            .points()
            .iter()
            .enumerate()
            .map(|(index, &point)| {
                let there = Self::screen_of(plot, point);
                (index, (there.x - at.x).abs().max((there.y - at.y).abs()))
            })
            .filter(|&(_, off)| off <= reach)
            .min_by_key(|&(_, off)| off)
            .map(|(index, _)| index)
    }

    /// Feed a pointer event: a press takes the point within reach or adds one
    /// where it lands, and the point follows the pointer until let go — away,
    /// if it was carried out of the graph.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        bounds: Rect,
        curve: &Curve,
        scale: Scale,
        damage: &mut Region,
    ) -> Option<CurveEdit> {
        if let InputEvent::PointerMoved { to } = event {
            self.pointer = *to;
        }
        if !self.enabled {
            return None;
        }
        let plot = Self::plot(bounds, scale);
        match event {
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            } if bounds.contains(self.pointer) => {
                damage.add(bounds);
                if let Some(index) = Self::point_at(curve, self.pointer, plot, scale) {
                    self.selected = Some(index);
                    self.drag = Some(Drag {
                        index,
                        outside: false,
                    });
                    return Some(CurveEdit::Chose);
                }
                let mut added = *curve;
                let index = added.add(Self::level_of(self.pointer, plot))?;
                self.selected = Some(index);
                self.drag = Some(Drag {
                    index,
                    outside: false,
                });
                Some(CurveEdit::Changed {
                    curve: added,
                    settled: false,
                })
            }
            InputEvent::PointerMoved { to } => {
                let drag = self.drag?;
                let out = to_i32(scale.scale_length(OUT));
                let outside = to.x < plot.left() - out
                    || to.x > plot.right() + out
                    || to.y < plot.top() - out
                    || to.y > plot.bottom() + out;
                let mut moved = *curve;
                moved.set(drag.index, Self::level_of(*to, plot))?;
                self.drag = Some(Drag { outside, ..drag });
                damage.add(bounds);
                Some(CurveEdit::Changed {
                    curve: moved,
                    settled: false,
                })
            }
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            } => {
                let drag = self.drag.take()?;
                damage.add(bounds);
                let mut settled = *curve;
                if drag.outside && settled.remove(drag.index) {
                    self.selected = None;
                }
                Some(CurveEdit::Changed {
                    curve: settled,
                    settled: true,
                })
            }
            _ => None,
        }
    }

    /// Feed a key while the editor has the keyboard: the arrows nudge the
    /// chosen point a level, ten with Shift; Delete takes it away; Page Up and
    /// Page Down choose the point before and after it.
    pub fn on_key(
        &mut self,
        stroke: Keystroke,
        bounds: Rect,
        curve: &Curve,
        damage: &mut Region,
    ) -> Option<CurveEdit> {
        if !self.focused || !self.enabled {
            return None;
        }
        let count = curve.points().len();
        if let Key::Named(NamedKey::PageUp | NamedKey::PageDown) = stroke.key {
            let forward = stroke.key == Key::Named(NamedKey::PageDown);
            let next = match (self.selected, forward) {
                (None, _) => 0,
                (Some(at), true) => (at + 1) % count,
                (Some(at), false) => (at + count - 1) % count,
            };
            self.selected = Some(next);
            damage.add(bounds);
            return Some(CurveEdit::Chose);
        }
        let index = self.selected.filter(|&index| index < count)?;
        let mut changed = *curve;
        if let Key::Named(NamedKey::Delete | NamedKey::Backspace) = stroke.key {
            if !changed.remove(index) {
                return None;
            }
            self.selected = Some(index.min(count - 2));
        } else {
            let step = if stroke.modifiers.shift { 10 } else { 1 };
            let (input, output) = curve.points()[index];
            let shift = |level: u8, by: i32| {
                u8::try_from((i32::from(level) + by).clamp(0, 255)).unwrap_or(level)
            };
            let to = match stroke.key {
                Key::Named(NamedKey::Left) => (shift(input, -step), output),
                Key::Named(NamedKey::Right) => (shift(input, step), output),
                Key::Named(NamedKey::Up) => (input, shift(output, step)),
                Key::Named(NamedKey::Down) => (input, shift(output, -step)),
                _ => return None,
            };
            changed.set(index, to)?;
            if changed == *curve {
                return None;
            }
        }
        damage.add(bounds);
        Some(CurveEdit::Changed {
            curve: changed,
            settled: true,
        })
    }

    /// Paint the graph: its grid and the even diagonal, the histogram of
    /// `plot` behind, the curve, and its points.
    pub fn render(
        &self,
        surface: &mut Surface,
        bounds: Rect,
        curve: &Curve,
        histogram: Option<(&Histogram, Plot)>,
        scale: Scale,
        theme: &Theme,
    ) {
        let palette = theme.palette();
        let plot = Self::plot(bounds, scale);
        fill_area(surface, plot, Color::from(palette.document));
        let quiet = palette.on_surface_muted;
        if let Some((histogram, shown)) = histogram {
            for column in 0..plot.width {
                let tall = u32::from(histogram.column(shown, column, plot.width));
                let height =
                    u32::try_from(u64::from(plot.height) * u64::from(tall) / 1000).unwrap_or(0);
                let x = plot.left().saturating_add_unsigned(column);
                blend_area(
                    surface,
                    Rect::new(x, plot.bottom() - to_i32(height), 1, height),
                    quiet.with_alpha(HISTOGRAM_ALPHA),
                );
            }
        }
        let grid = quiet.with_alpha(HISTOGRAM_ALPHA);
        for quarter in 1..4u32 {
            let x = plot
                .left()
                .saturating_add_unsigned(plot.width * quarter / 4);
            let y = plot
                .top()
                .saturating_add_unsigned(plot.height * quarter / 4);
            blend_area(surface, Rect::new(x, plot.top(), 1, plot.height), grid);
            blend_area(surface, Rect::new(plot.left(), y, plot.width, 1), grid);
        }
        let weight = to_i32(scale.scale_length(1).max(1)) * SUBPIXEL;
        let line = |level: u8, value: f64| {
            let column = Self::screen_of(plot, (level, 0)).x;
            let span = f64::from(plot.height.saturating_sub(1));
            let y = f64::from(plot.bottom() - 1) - value / 255.0 * span;
            (
                column * SUBPIXEL + SUBPIXEL / 2,
                tairix_util::mathf::round_i32(y * f64::from(SUBPIXEL)) + SUBPIXEL / 2,
            )
        };
        let even = [line(0, 0.0), line(255, 255.0)];
        surface.stroke_polyline(&even, weight, Color::from(quiet));
        let table = curve.table();
        let mut traced = [(0, 0); 256];
        for (slot, (level, &value)) in traced.iter_mut().zip((0u8..=255).zip(table.iter())) {
            *slot = line(level, f64::from(value));
        }
        let ink = if self.enabled {
            Color::from(palette.on_surface)
        } else {
            Color::from(quiet)
        };
        surface.stroke_polyline(&traced, weight * 2, ink);
        let side = scale.scale_length(POINT).max(3);
        for (index, &point) in curve.points().iter().enumerate() {
            let at = Self::screen_of(plot, point);
            let square = Rect::new(at.x - to_i32(side / 2), at.y - to_i32(side / 2), side, side);
            let chosen = self.selected == Some(index);
            let leaving = self
                .drag
                .is_some_and(|drag| drag.index == index && drag.outside);
            fill_area(surface, square, ink);
            if !chosen || leaving {
                fill_area(
                    surface,
                    square.inset(scale.scale_length(2).max(1)),
                    Color::from(palette.document),
                );
            }
            if chosen && self.focused && !leaving {
                fill_area(surface, square.inset(1), Color::from(palette.accent));
            }
        }
    }
}

#[cfg(test)]
#[path = "curve_graph_tests.rs"]
mod tests;

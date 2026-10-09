//! A track of handles: up to three markers along a groove, each standing at a
//! value, moved by the pointer or by the arrow keys; the groove may show what
//! its values mean as a sweep of colours.
//!
//! The track reports where a handle was asked to go and holds nothing back:
//! its owner keeps the handles in order and sets them where they may stand.

use tairix_controls::{fill_area, Keystroke};
use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_raster::{Color, Surface, SUBPIXEL};
use tairix_theme::Theme;

/// The most handles a track holds.
pub const MOST_HANDLES: usize = 3;

/// A track's height, in logical pixels: the groove and the handles under it.
pub const TRACK_HEIGHT: u32 = 18;

/// The groove's height, in logical pixels.
const GROOVE: u32 = 8;

/// How far a press may land from a handle and still take it, in logical
/// pixels.
const REACH: u32 = 8;

/// What moving a handle came to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Moved {
    /// Which handle.
    pub handle: usize,
    /// Where it was asked to stand.
    pub value: i32,
    /// Whether the move is over: a release, or a key's step.
    pub settled: bool,
}

/// A track of handles.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Track {
    values: [i32; MOST_HANDLES],
    fills: [Color; MOST_HANDLES],
    count: usize,
    least: i32,
    most: i32,
    /// The handle the keys move.
    active: usize,
    focused: bool,
    enabled: bool,
    dragging: bool,
    pointer: Point,
}

impl Track {
    /// A track from `least` to `most` with a handle at each of `handles`,
    /// filled with its colour; past [`MOST_HANDLES`] are left off.
    #[must_use]
    pub fn new(least: i32, most: i32, handles: &[(i32, Color)]) -> Self {
        let (least, most) = (least.min(most), least.max(most));
        let mut values = [least; MOST_HANDLES];
        let mut fills = [Color::rgba(0, 0, 0, 255); MOST_HANDLES];
        let count = handles.len().min(MOST_HANDLES);
        for ((value, fill), &(at, colour)) in values.iter_mut().zip(&mut fills).zip(handles) {
            *value = at.clamp(least, most);
            *fill = colour;
        }
        Self {
            values,
            fills,
            count,
            least,
            most,
            active: 0,
            focused: false,
            enabled: true,
            dragging: false,
            pointer: Point::ORIGIN,
        }
    }

    /// How many handles it holds.
    #[must_use]
    pub const fn count(&self) -> usize {
        self.count
    }

    /// Where handle `handle` stands.
    #[must_use]
    pub fn value(&self, handle: usize) -> Option<i32> {
        self.values[..self.count].get(handle).copied()
    }

    /// Stand handle `handle` at `value`, held to the track, answering whether
    /// it moved; the owner reports the repaint.
    pub fn set(&mut self, handle: usize, value: i32) -> bool {
        let value = value.clamp(self.least, self.most);
        match self.values[..self.count].get_mut(handle) {
            Some(slot) if *slot != value => {
                *slot = value;
                true
            }
            _ => false,
        }
    }

    /// Give the keyboard to handle `handle`, or take it from the track.
    pub fn focus(&mut self, handle: Option<usize>) {
        self.focused = handle.is_some();
        if let Some(handle) = handle.filter(|&handle| handle < self.count) {
            self.active = handle;
        }
    }

    /// The handle the keys move, while the track has the keyboard.
    #[must_use]
    pub const fn focused(&self) -> Option<usize> {
        if self.focused {
            Some(self.active)
        } else {
            None
        }
    }

    /// Offer the track, or withhold it.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.dragging = false;
        }
    }

    /// Whether a handle is being dragged.
    #[must_use]
    pub const fn dragging(&self) -> bool {
        self.dragging
    }

    /// The groove within `bounds`, inset so a handle at either end is whole.
    fn groove(bounds: Rect, scale: Scale) -> Rect {
        let half = scale.scale_length(REACH / 2).max(1);
        Rect::new(
            bounds.left().saturating_add_unsigned(half),
            bounds.top(),
            bounds.width.saturating_sub(half * 2),
            scale.scale_length(GROOVE).max(1).min(bounds.height),
        )
    }

    /// The x a value stands at along `groove`.
    fn x_of(&self, value: i32, groove: Rect) -> i32 {
        let span = i64::from(self.most - self.least).max(1);
        let along =
            i64::from(value - self.least) * i64::from(groove.width.saturating_sub(1)) / span;
        groove.left() + i32::try_from(along).unwrap_or(0)
    }

    /// The value a pointer at `x` asks for along `groove`.
    fn value_at(&self, x: i32, groove: Rect) -> i32 {
        let width = i64::from(groove.width.saturating_sub(1)).max(1);
        let along = i64::from((x - groove.left()).clamp(0, to_i32(groove.width)));
        let span = i64::from(self.most - self.least);
        let value = i64::from(self.least) + (along * span + width / 2) / width;
        i32::try_from(value)
            .unwrap_or(self.least)
            .clamp(self.least, self.most)
    }

    /// The handle a press at `x` within `bounds` takes.
    #[must_use]
    pub fn handle_near(&self, x: i32, bounds: Rect, scale: Scale) -> usize {
        self.nearest(x, Self::groove(bounds, scale))
    }

    /// The handle nearest `x`; of handles standing together, the one past
    /// which the pointer lies, so the higher can still be dragged up.
    fn nearest(&self, x: i32, groove: Rect) -> usize {
        let mut best = 0;
        let mut best_off = i32::MAX;
        for handle in 0..self.count {
            let at = self.x_of(self.values[handle], groove);
            let off = (x - at).abs();
            if off < best_off || (off == best_off && x > at) {
                best = handle;
                best_off = off;
            }
        }
        best
    }

    /// Feed a pointer event: a press within the track takes the nearest
    /// handle there, and it follows the pointer until let go.
    pub fn on_pointer(
        &mut self,
        event: &InputEvent,
        bounds: Rect,
        scale: Scale,
        damage: &mut Region,
    ) -> Option<Moved> {
        if let InputEvent::PointerMoved { to } = event {
            self.pointer = *to;
        }
        if !self.enabled || self.count == 0 {
            return None;
        }
        let groove = Self::groove(bounds, scale);
        let reach = to_i32(scale.scale_length(REACH));
        match event {
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            } if bounds.contains(self.pointer)
                && self.pointer.x >= groove.left() - reach
                && self.pointer.x <= groove.right() + reach =>
            {
                self.active = self.nearest(self.pointer.x, groove);
                self.dragging = true;
                self.step_to(self.value_at(self.pointer.x, groove), false, bounds, damage)
            }
            InputEvent::PointerMoved { to } if self.dragging => {
                self.step_to(self.value_at(to.x, groove), false, bounds, damage)
            }
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            } if self.dragging => {
                self.dragging = false;
                let value = self.values[self.active];
                Some(Moved {
                    handle: self.active,
                    value,
                    settled: true,
                })
            }
            _ => None,
        }
    }

    /// Feed a key while the track has the keyboard: Left and Right step the
    /// handle by one, ten with Shift; Home and End take it to the ends.
    pub fn on_key(
        &mut self,
        stroke: Keystroke,
        bounds: Rect,
        damage: &mut Region,
    ) -> Option<Moved> {
        if !self.focused || !self.enabled || self.count == 0 {
            return None;
        }
        let step = if stroke.modifiers.shift { 10 } else { 1 };
        let value = self.values[self.active];
        let to = match stroke.key {
            Key::Named(NamedKey::Left | NamedKey::Down) => value - step,
            Key::Named(NamedKey::Right | NamedKey::Up) => value + step,
            Key::Named(NamedKey::Home) => self.least,
            Key::Named(NamedKey::End) => self.most,
            _ => return None,
        };
        self.step_to(to, true, bounds, damage)
    }

    /// Stand the active handle at `value`; nothing where it stands there.
    fn step_to(
        &mut self,
        value: i32,
        settled: bool,
        bounds: Rect,
        damage: &mut Region,
    ) -> Option<Moved> {
        let value = value.clamp(self.least, self.most);
        if value == self.values[self.active] {
            return None;
        }
        self.values[self.active] = value;
        damage.add(bounds);
        Some(Moved {
            handle: self.active,
            value,
            settled,
        })
    }

    /// Paint the groove — swept by `sweep`, given each column's place along
    /// it in thousandths, or plain — and the handles beneath it.
    pub fn render(
        &self,
        surface: &mut Surface,
        bounds: Rect,
        scale: Scale,
        theme: &Theme,
        sweep: Option<&dyn Fn(u32) -> Color>,
    ) {
        let palette = theme.palette();
        let groove = Self::groove(bounds, scale);
        let rim = Color::from(palette.rim);
        fill_area(surface, groove, rim);
        let inner = groove.inset(1);
        match sweep {
            Some(sweep) => {
                let width = inner.width.max(1);
                for column in 0..inner.width {
                    let along = u32::try_from(
                        u64::from(column) * 1000 / u64::from(width.saturating_sub(1).max(1)),
                    )
                    .unwrap_or(1000);
                    let x = inner.left().saturating_add_unsigned(column);
                    fill_area(
                        surface,
                        Rect::new(x, inner.top(), 1, inner.height),
                        sweep(along),
                    );
                }
            }
            None => fill_area(surface, inner, Color::from(palette.scroll_track)),
        }
        let side = scale.scale_length(REACH).max(4);
        let top = groove.bottom();
        let height = u32::try_from(bounds.bottom() - top).unwrap_or(0).min(side);
        for handle in 0..self.count {
            let x = self.x_of(self.values[handle], groove);
            let picked = self.focused && handle == self.active;
            let outline = if picked {
                Color::from(palette.accent)
            } else {
                Color::from(palette.on_surface)
            };
            let half = to_i32(side / 2);
            let tip = (x * SUBPIXEL + SUBPIXEL / 2, top * SUBPIXEL);
            let base = (top + to_i32(height)) * SUBPIXEL;
            let outer = [
                tip,
                ((x + half + 1) * SUBPIXEL, base),
                ((x - half) * SUBPIXEL, base),
            ];
            surface.fill_polygon_subpixel(&outer, outline);
            let inset = SUBPIXEL * 3 / 2;
            let inner = [
                (tip.0, tip.1 + inset * 2),
                ((x + half + 1) * SUBPIXEL - inset * 2, base - inset),
                ((x - half) * SUBPIXEL + inset * 2, base - inset),
            ];
            let fill = if self.enabled {
                self.fills[handle]
            } else {
                Color::from(palette.on_surface_muted)
            };
            surface.fill_polygon_subpixel(&inner, fill);
        }
    }
}

#[cfg(test)]
#[path = "track_tests.rs"]
mod tests;

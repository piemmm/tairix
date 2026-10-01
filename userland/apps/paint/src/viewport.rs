//! Where the picture sits in its window: how far it is magnified, the shape
//! of its pixels, and how far it is scrolled.
//!
//! One picture pixel spans `num × aspect / den` screen pixels on each axis,
//! so a sprite whose pixels are twice as tall as wide is shown so. A picture
//! smaller than the canvas is centred in it; a larger one scrolls, in screen
//! pixels.

use tairix_geometry::{Point, Rect};

use crate::shape::{Bounds, FX};

/// The magnifications, lowest first, as `(num, den)`.
pub const ZOOMS: [(u32, u32); 16] = [
    (1, 16),
    (1, 8),
    (1, 4),
    (1, 2),
    (1, 1),
    (2, 1),
    (3, 1),
    (4, 1),
    (6, 1),
    (8, 1),
    (12, 1),
    (16, 1),
    (24, 1),
    (32, 1),
    (48, 1),
    (64, 1),
];

/// The magnification a rung of [`ZOOMS`] shows at, in percent of actual
/// size.
#[must_use]
pub const fn percent_of((num, den): (u32, u32)) -> u32 {
    num * 100 / den
}

/// The rung of [`ZOOMS`] showing a picture pixel as a screen pixel.
pub const ACTUAL: usize = 4;

/// Screen pixels a picture pixel spans from which a grid between pixels is
/// worth drawing.
pub const GRID_FROM: u64 = 8;

/// How the picture is shown.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Viewport {
    zoom: usize,
    aspect: (u32, u32),
    scroll: (u64, u64),
}

impl Viewport {
    /// A picture whose pixels are `aspect` wide to high, at actual size and
    /// scrolled to its top left.
    #[must_use]
    pub const fn new(aspect: (u32, u32)) -> Self {
        Self {
            zoom: ACTUAL,
            aspect,
            scroll: (0, 0),
        }
    }

    /// The rung of [`ZOOMS`] in use.
    #[must_use]
    pub const fn zoom(&self) -> usize {
        self.zoom
    }

    /// The magnification, in percent of actual size.
    #[must_use]
    pub const fn percent(&self) -> u32 {
        percent_of(ZOOMS[self.zoom])
    }

    /// How far the picture is scrolled, in screen pixels.
    #[must_use]
    pub const fn scroll(&self) -> (u64, u64) {
        self.scroll
    }

    /// Screen pixels a picture pixel spans across and down, as numerators over
    /// the shared denominator last.
    #[must_use]
    pub const fn span(&self) -> (u64, u64, u64) {
        let (num, den) = ZOOMS[self.zoom];
        (
            num as u64 * self.aspect.0 as u64,
            num as u64 * self.aspect.1 as u64,
            den as u64,
        )
    }

    /// Whole screen pixels one picture pixel spans across and down, or `0`
    /// when it spans less than one.
    #[must_use]
    pub const fn pixel_span(&self) -> (u64, u64) {
        let (across, down, den) = self.span();
        (across / den, down / den)
    }

    /// Screen pixels the whole picture spans, rounded up.
    #[must_use]
    pub const fn extent(&self, picture: (u32, u32)) -> (u64, u64) {
        let (across, down, den) = self.span();
        (
            (picture.0 as u64 * across).div_ceil(den),
            (picture.1 as u64 * down).div_ceil(den),
        )
    }

    /// Where the picture's top left falls on screen, for a canvas at `area`.
    #[must_use]
    pub fn origin(&self, picture: (u32, u32), area: Rect) -> (i64, i64) {
        let (width, height) = self.extent(picture);
        let place = |start: i32, room: u32, extent: u64, scroll: u64| {
            let room = u64::from(room);
            if extent < room {
                i64::from(start) + i64::try_from((room - extent) / 2).unwrap_or(0)
            } else {
                i64::from(start) - i64::try_from(scroll).unwrap_or(i64::MAX)
            }
        };
        (
            place(area.left(), area.width, width, self.scroll.0),
            place(area.top(), area.height, height, self.scroll.1),
        )
    }

    /// The picture position under the centre of screen pixel `point`, in
    /// 256ths of a picture pixel; off the picture it is simply outside its
    /// bounds.
    #[must_use]
    pub fn to_picture(&self, point: Point, picture: (u32, u32), area: Rect) -> (i64, i64) {
        let (ox, oy) = self.origin(picture, area);
        let (across, down, den) = self.span();
        let map = |at: i32, origin: i64, span: u64| {
            let offset = i128::from(i64::from(at) - origin) * 2 + 1;
            let fx =
                (offset * i128::from(den) * i128::from(FX)).div_euclid(2 * i128::from(span.max(1)));
            i64::try_from(fx).unwrap_or(i64::MAX)
        };
        (map(point.x, ox, across), map(point.y, oy, down))
    }

    /// The picture pixel under screen pixel `point`, if it is on the picture.
    #[must_use]
    pub fn pixel_at(&self, point: Point, picture: (u32, u32), area: Rect) -> Option<(u32, u32)> {
        if !area.contains(point) {
            return None;
        }
        let (x, y) = self.to_picture(point, picture, area);
        let (x, y) = (x.div_euclid(FX), y.div_euclid(FX));
        let x = u32::try_from(x).ok().filter(|&x| x < picture.0)?;
        let y = u32::try_from(y).ok().filter(|&y| y < picture.1)?;
        Some((x, y))
    }

    /// The screen pixels `bounds` of the picture fall on, clipped to `area`.
    #[must_use]
    pub fn to_screen(&self, bounds: Bounds, picture: (u32, u32), area: Rect) -> Rect {
        let span = self.screen_span(bounds, picture, area);
        let clamp = |value: i64| {
            i32::try_from(value.clamp(i64::from(i32::MIN), i64::from(i32::MAX))).unwrap_or(0)
        };
        let (x0, y0) = (clamp(span.x0), clamp(span.y0));
        let width = u32::try_from(clamp(span.x1).saturating_sub(x0)).unwrap_or(0);
        let height = u32::try_from(clamp(span.y1).saturating_sub(y0)).unwrap_or(0);
        Rect::new(x0, y0, width, height).intersection(&area)
    }

    /// The screen pixels `bounds` of the picture fall on, wherever they lie
    /// for a canvas at `area`, `[x0, x1) × [y0, y1)`.
    #[must_use]
    pub fn screen_span(&self, bounds: Bounds, picture: (u32, u32), area: Rect) -> Bounds {
        if bounds.is_empty() {
            return Bounds {
                x0: 0,
                y0: 0,
                x1: 0,
                y1: 0,
            };
        }
        let (ox, oy) = self.origin(picture, area);
        let (across, down, den) = self.span();
        let den = i128::from(den);
        let low = |at: i64, origin: i64, span: u64| {
            i128::from(origin) + (i128::from(at) * i128::from(span)).div_euclid(den)
        };
        let high = |at: i64, origin: i64, span: u64| {
            let scaled = i128::from(at) * i128::from(span);
            i128::from(origin) + (scaled + den - 1).div_euclid(den)
        };
        let narrow = |value: i128| {
            i64::try_from(value.clamp(i128::from(i64::MIN), i128::from(i64::MAX))).unwrap_or(0)
        };
        Bounds {
            x0: narrow(low(bounds.x0, ox, across)),
            y0: narrow(low(bounds.y0, oy, down)),
            x1: narrow(high(bounds.x1, ox, across)),
            y1: narrow(high(bounds.y1, oy, down)),
        }
    }

    /// Scroll to `(x, y)` screen pixels into the picture, kept to where the
    /// picture still fills the canvas; answers whether anything moved.
    pub fn scroll_to(&mut self, x: u64, y: u64, picture: (u32, u32), area: Rect) -> bool {
        let (width, height) = self.extent(picture);
        let most = |extent: u64, room: u32| extent.saturating_sub(u64::from(room));
        let wanted = (
            x.min(most(width, area.width)),
            y.min(most(height, area.height)),
        );
        let moved = wanted != self.scroll;
        self.scroll = wanted;
        moved
    }

    /// Keep the scroll within the picture's extent at a canvas of `area`.
    pub fn settle(&mut self, picture: (u32, u32), area: Rect) -> bool {
        let (x, y) = self.scroll;
        self.scroll_to(x, y, picture, area)
    }

    /// Magnify to rung `zoom`, the picture point under `anchor` staying
    /// under it; answers whether the rung changed.
    pub fn zoom_to(&mut self, zoom: usize, anchor: Point, picture: (u32, u32), area: Rect) -> bool {
        let zoom = zoom.min(ZOOMS.len() - 1);
        if zoom == self.zoom {
            return false;
        }
        let (px, py) = self.to_picture(anchor, picture, area);
        self.zoom = zoom;
        let (across, down, den) = self.span();
        let (width, height) = self.extent(picture);
        let aim = |fx: i64, span: u64, at: i32, start: i32, extent: u64, room: u32| {
            if extent < u64::from(room) {
                return 0;
            }
            let screen = i128::from(fx) * i128::from(span) / (i128::from(den) * i128::from(FX));
            let scroll = screen - i128::from(i64::from(at) - i64::from(start));
            u64::try_from(scroll.max(0)).unwrap_or(0)
        };
        let x = aim(px, across, anchor.x, area.left(), width, area.width);
        let y = aim(py, down, anchor.y, area.top(), height, area.height);
        self.scroll_to(x, y, picture, area);
        true
    }

    /// The greatest rung at which the whole picture fits `area`, never above
    /// actual size.
    #[must_use]
    pub fn fitting(&self, picture: (u32, u32), area: Rect) -> usize {
        let mut probe = *self;
        (0..=ACTUAL)
            .rev()
            .find(|&zoom| {
                probe.zoom = zoom;
                let (width, height) = probe.extent(picture);
                width <= u64::from(area.width) && height <= u64::from(area.height)
            })
            .unwrap_or(0)
    }

    /// Show pixels of shape `aspect`.
    pub fn set_aspect(&mut self, aspect: (u32, u32)) {
        self.aspect = (aspect.0.max(1), aspect.1.max(1));
    }
}

#[cfg(test)]
#[path = "viewport_tests.rs"]
mod tests;

//! Where the picture sits in its window: how far it is magnified, the shape
//! of its pixels, and how far it is scrolled.
//!
//! The magnification is continuous, so a pinch zooms smoothly; the ladder of
//! [`ZOOMS`] is what the stepping commands move along. One picture pixel spans
//! `zoom × aspect` screen pixels on each axis, so a sprite whose pixels are
//! twice as tall as wide is shown so. A picture smaller than the canvas is
//! centred in it; a larger one scrolls, in screen pixels.

use tairix_abi::touch::PINCH_SCALE_ONE;
use tairix_geometry::{saturate_i32, Point, Rect};

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

/// A magnification, in 4096ths of actual size: fine enough that a pinch
/// zooms smoothly, and every rung of [`ZOOMS`] a whole number of them.
#[derive(Copy, Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Zoom(u32);

impl Zoom {
    const UNIT: u32 = 4096;

    /// The least magnification offered.
    pub const LEAST: Self = Self::of(ZOOMS[0]);

    /// The most magnification offered.
    pub const MOST: Self = Self::of(ZOOMS[ZOOMS.len() - 1]);

    /// The magnification a rung of [`ZOOMS`] shows at.
    #[must_use]
    pub const fn of((num, den): (u32, u32)) -> Self {
        Self(num * Self::UNIT / den)
    }

    /// This magnification times a pinch's `scale`, in 16.16 fixed point,
    /// held to the ladder's ends.
    #[must_use]
    pub fn scaled(self, scale: u32) -> Self {
        let zoom = u64::from(self.0) * u64::from(scale) / u64::from(PINCH_SCALE_ONE);
        Self(
            u32::try_from(zoom)
                .unwrap_or(u32::MAX)
                .clamp(Self::LEAST.0, Self::MOST.0),
        )
    }

    /// In percent of actual size.
    #[must_use]
    pub const fn percent(self) -> u32 {
        self.0 * 100 / Self::UNIT
    }
}

/// The rung of [`ZOOMS`] showing a picture pixel as a screen pixel.
pub const ACTUAL: usize = 4;

/// Screen pixels a picture pixel spans from which a grid between pixels is
/// worth drawing.
pub const GRID_FROM: u64 = 8;

/// How the picture is shown.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Viewport {
    zoom: Zoom,
    aspect: (u32, u32),
    scroll: (u64, u64),
}

impl Viewport {
    /// A picture whose pixels are `aspect` wide to high, at actual size and
    /// scrolled to its top left.
    #[must_use]
    pub const fn new(aspect: (u32, u32)) -> Self {
        Self {
            zoom: Zoom::of(ZOOMS[ACTUAL]),
            aspect,
            scroll: (0, 0),
        }
    }

    /// The magnification in use.
    #[must_use]
    pub const fn zoom(&self) -> Zoom {
        self.zoom
    }

    /// The rung of [`ZOOMS`] the magnification is exactly at, if it is at
    /// one.
    #[must_use]
    pub fn rung(&self) -> Option<usize> {
        ZOOMS.iter().position(|&rung| Zoom::of(rung) == self.zoom)
    }

    /// The rung `steps` rungs above the magnification in use, or below it
    /// for a negative count: from between two rungs, the first step lands on
    /// the nearer one in its direction.
    #[must_use]
    pub fn rung_beside(&self, steps: i64) -> usize {
        let last = ZOOMS.len() - 1;
        let further = usize::try_from(steps.unsigned_abs().saturating_sub(1)).unwrap_or(last);
        if steps >= 0 {
            ZOOMS
                .iter()
                .position(|&rung| Zoom::of(rung) > self.zoom)
                .map_or(last, |above| above.saturating_add(further).min(last))
        } else {
            ZOOMS
                .iter()
                .rposition(|&rung| Zoom::of(rung) < self.zoom)
                .map_or(0, |below| below.saturating_sub(further))
        }
    }

    /// The magnification, in percent of actual size.
    #[must_use]
    pub const fn percent(&self) -> u32 {
        self.zoom.percent()
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
        (
            self.zoom.0 as u64 * self.aspect.0 as u64,
            self.zoom.0 as u64 * self.aspect.1 as u64,
            Zoom::UNIT as u64,
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

    /// The screen pixel picture position `at`, in 256ths of a picture pixel,
    /// falls in, for a canvas at `area`.
    #[must_use]
    pub fn screen_of(&self, at: (i64, i64), picture: (u32, u32), area: Rect) -> (i64, i64) {
        let (ox, oy) = self.origin(picture, area);
        let (across, down, den) = self.span();
        let map = |at: i64, origin: i64, span: u64| {
            let scaled =
                (i128::from(at) * i128::from(span)).div_euclid(i128::from(den) * i128::from(FX));
            i64::try_from(i128::from(origin) + scaled).unwrap_or(if at < 0 {
                i64::MIN
            } else {
                i64::MAX
            })
        };
        (map(at.0, ox, across), map(at.1, oy, down))
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
        screen_rect(self.screen_span(bounds, picture, area)).intersection(&area)
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

    /// Scroll by `(dx, dy)` screen pixels, kept to where the picture still
    /// fills the canvas; answers whether anything moved.
    pub fn scroll_by(&mut self, dx: i64, dy: i64, picture: (u32, u32), area: Rect) -> bool {
        let shifted = |at: u64, by: i64| at.saturating_add_signed(by);
        self.scroll_to(
            shifted(self.scroll.0, dx),
            shifted(self.scroll.1, dy),
            picture,
            area,
        )
    }

    /// Keep the scroll within the picture's extent at a canvas of `area`.
    pub fn settle(&mut self, picture: (u32, u32), area: Rect) -> bool {
        let (x, y) = self.scroll;
        self.scroll_to(x, y, picture, area)
    }

    /// Magnify to rung `rung` of [`ZOOMS`], the picture point under `anchor`
    /// staying under it; answers whether the magnification changed.
    pub fn zoom_to(&mut self, rung: usize, anchor: Point, picture: (u32, u32), area: Rect) -> bool {
        self.magnify(
            Zoom::of(ZOOMS[rung.min(ZOOMS.len() - 1)]),
            anchor,
            picture,
            area,
        )
    }

    /// Magnify to `zoom`, the picture point under `anchor` staying under it;
    /// answers whether the magnification changed.
    pub fn magnify(&mut self, zoom: Zoom, anchor: Point, picture: (u32, u32), area: Rect) -> bool {
        let zoom = zoom.clamp(Zoom::LEAST, Zoom::MOST);
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

    /// Magnify so picture pixels `bounds` fill `area` as far as they fit,
    /// held to the ladder's ends, and scroll them to its middle; answers
    /// whether anything moved.
    pub fn frame(&mut self, bounds: Bounds, picture: (u32, u32), area: Rect) -> bool {
        if bounds.is_empty() || area.is_empty() {
            return false;
        }
        let fit = |room: u32, pixels: i64, aspect: u32| {
            let pixels = u64::try_from(pixels).unwrap_or(1).max(1);
            u64::from(room) * u64::from(Zoom::UNIT) / (pixels * u64::from(aspect.max(1)))
        };
        let zoom = fit(area.width, bounds.x1 - bounds.x0, self.aspect.0).min(fit(
            area.height,
            bounds.y1 - bounds.y0,
            self.aspect.1,
        ));
        let before = (self.zoom, self.scroll);
        self.zoom = Zoom(u32::try_from(zoom).unwrap_or(u32::MAX)).clamp(Zoom::LEAST, Zoom::MOST);
        let (across, down, den) = self.span();
        let centre = |low: i64, high: i64, span: u64, room: u32| {
            let middle = i128::from(low + high) * i128::from(span) / (2 * i128::from(den));
            u64::try_from((middle - i128::from(room / 2)).max(0)).unwrap_or(0)
        };
        let x = centre(bounds.x0, bounds.x1, across, area.width);
        let y = centre(bounds.y0, bounds.y1, down, area.height);
        self.scroll_to(x, y, picture, area);
        (self.zoom, self.scroll) != before
    }

    /// The greatest rung at which the whole picture fits `area`, never above
    /// actual size.
    #[must_use]
    pub fn fitting(&self, picture: (u32, u32), area: Rect) -> usize {
        let mut probe = *self;
        (0..=ACTUAL)
            .rev()
            .find(|&rung| {
                probe.zoom = Zoom::of(ZOOMS[rung]);
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

/// The screen rectangle of screen pixels `span`, held to what a screen
/// coordinate reaches.
#[must_use]
pub fn screen_rect(span: Bounds) -> Rect {
    let (x0, y0) = (saturate_i32(span.x0), saturate_i32(span.y0));
    let width = u32::try_from(saturate_i32(span.x1).saturating_sub(x0)).unwrap_or(0);
    let height = u32::try_from(saturate_i32(span.y1).saturating_sub(y0)).unwrap_or(0);
    Rect::new(x0, y0, width, height)
}

#[cfg(test)]
#[path = "viewport_tests.rs"]
mod tests;

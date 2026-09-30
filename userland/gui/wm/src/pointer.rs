//! The pointer overlay: the cursor, and the aids drawn with it — a trail of
//! fading copies where it has just been, and a halo of rings that shows where
//! it is.
//!
//! All of it is composed after every window, so no frost reads it. Its damage
//! is derived when a frame is composed rather than when a part changes: each
//! part's footprint as the last composite drew it is diffed against the one it
//! would draw now, so any number of changes between two frames recomposes only
//! what each part left and where it now lies. A halo's footprint is its band
//! cut into slabs, never the square around it, because the rings are thin and
//! everything inside them is left as it was.

use tairix_cursor::{CursorImage, PlacedCursor};
use tairix_inline::ArrayVec;
use tairix_raster::{Ring, RingInk};

use crate::color::{Color, Pixel};
use crate::geometry::{Point, Rect};
use crate::surface::{row, Surface};

/// The most copies of the pointer a trail draws.
pub const MAX_GHOSTS: usize = 8;

/// The most rings a halo draws.
pub const MAX_HALO_RINGS: usize = 4;

/// How many slabs a ring's band is cut into for its damage: enough that the
/// slabs follow the curve closely, few enough that a whole halo stays a
/// handful of rectangles.
const RING_SLABS: u32 = 8;

/// The most rectangles a halo's footprint can take: every ring its own band,
/// every slab of it split either side of the hole.
const MAX_HALO_COVER: usize = MAX_HALO_RINGS * 2 * RING_SLABS as usize;

/// The most sprites the overlay draws at once: the trail, the halo, and the
/// cursor on top.
pub(crate) const MAX_SPRITES: usize = MAX_GHOSTS + 2;

/// The most rectangles one composite can owe the overlay: every part where it
/// was and where it is.
pub(crate) const MAX_OWED: usize = 2 * (1 + MAX_GHOSTS + MAX_HALO_COVER);

/// A copy of the pointer drawn where it recently was.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Ghost {
    /// Where the pointer's hotspot was.
    pub at: Point,
    /// How strongly the copy is drawn; `u8::MAX` is as strongly as the pointer.
    pub opacity: u8,
}

/// One ring of a [`Halo`]: a band `width` pixels wide whose outer edge lies
/// `radius` pixels from the pointer.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct HaloRing {
    /// How far the band's outer edge is from the pointer, in pixels.
    pub radius: u32,
    /// How wide the band is, in pixels.
    pub width: u32,
    /// What the band is drawn in.
    pub color: Color,
}

impl HaloRing {
    /// How far the band's inner edge is from the pointer.
    const fn inner(self) -> u32 {
        self.radius.saturating_sub(self.width)
    }
}

/// Rings drawn around the pointer to show where it is, bottom to top.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Halo {
    rings: ArrayVec<HaloRing, MAX_HALO_RINGS>,
}

impl Halo {
    /// A halo of no rings, which draws nothing.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            rings: ArrayVec::new(),
        }
    }

    /// Lay `ring` over the rings already held, answering whether there was
    /// room for it. A ring that draws nothing is not held.
    pub fn push(&mut self, ring: HaloRing) -> bool {
        if ring.radius == 0 || ring.width == 0 || ring.color.a == 0 {
            return true;
        }
        self.rings.try_push(ring).is_ok()
    }

    /// The rings, bottom to top.
    #[must_use]
    pub fn rings(&self) -> &[HaloRing] {
        self.rings.as_slice()
    }

    /// The largest ring's radius: how far from the pointer the halo reaches.
    fn reach(&self) -> u32 {
        self.rings.iter().map(|ring| ring.radius).max().unwrap_or(0)
    }
}

/// One sprite's pixels on one screen row: where the first of them lies and how
/// strongly they are laid.
#[derive(Copy, Clone)]
pub(crate) struct SpriteRun<'a> {
    pub(crate) pixels: &'a [Pixel],
    pub(crate) left: i32,
    pub(crate) opacity: u8,
}

/// Something the overlay draws: a surface laid over `bounds` at `opacity`.
#[derive(Copy, Clone)]
pub(crate) struct Sprite<'a> {
    surface: &'a Surface,
    bounds: Rect,
    opacity: u8,
}

impl<'a> Sprite<'a> {
    /// The screen rectangle the sprite covers.
    pub(crate) const fn bounds(&self) -> Rect {
        self.bounds
    }

    /// The sprite's pixels on screen row `y`, or `None` off its rows.
    pub(crate) fn run(&self, y: i32) -> Option<SpriteRun<'a>> {
        let ly = u32::try_from(y.checked_sub(self.bounds.top())?).ok()?;
        (ly < self.bounds.height).then(|| SpriteRun {
            pixels: row(self.surface, ly),
            left: self.bounds.left(),
            opacity: self.opacity,
        })
    }

    /// The sprite's pixel at its own `(lx, ly)` at the strength it is laid, or
    /// `None` where it draws nothing.
    pub(crate) fn sample_local(&self, lx: u32, ly: u32) -> Option<Pixel> {
        let pixel = self.surface.get(lx, ly).filter(|pixel| pixel.a > 0)?;
        Some(pixel.scale_alpha(self.opacity))
    }
}

/// Which parts changed their pixels since the last composite, beyond moving.
#[derive(Copy, Clone, Default)]
struct Changed {
    cursor: bool,
    trail: bool,
    halo: bool,
}

/// Where each part lies on screen.
#[derive(Clone, Default, Eq, PartialEq)]
struct Footprint {
    cursor: Option<Rect>,
    trail: ArrayVec<Rect, MAX_GHOSTS>,
    halo: ArrayVec<Rect, MAX_HALO_COVER>,
}

impl Footprint {
    #[cfg(test)]
    fn rects(&self) -> impl Iterator<Item = Rect> + '_ {
        self.cursor
            .iter()
            .chain(self.trail.as_slice())
            .chain(self.halo.as_slice())
            .copied()
    }
}

/// The cursor and the aids drawn with it.
pub(crate) struct PointerOverlay {
    cursor: Option<PlacedCursor>,
    /// Where the pointer is, which the halo is centred on whether or not a
    /// cursor is installed.
    pointer: Point,
    /// Whether the whole overlay is withheld from the screen.
    hidden: bool,
    trail: ArrayVec<Ghost, MAX_GHOSTS>,
    halo: Halo,
    /// The halo as drawn, centred on the pointer: a square twice the widest
    /// reach it has had while shown, kept so a shrinking halo is redrawn in
    /// place rather than into a new buffer every frame.
    art: Option<Surface>,
    changed: Changed,
    /// Each part's footprint as of the last composite.
    drawn: Footprint,
}

impl PointerOverlay {
    /// An overlay with no cursor and no aids.
    pub(crate) const fn new() -> Self {
        Self {
            cursor: None,
            pointer: Point::ORIGIN,
            hidden: false,
            trail: ArrayVec::new(),
            halo: Halo::new(),
            art: None,
            changed: Changed {
                cursor: false,
                trail: false,
                halo: false,
            },
            drawn: Footprint {
                cursor: None,
                trail: ArrayVec::new(),
                halo: ArrayVec::new(),
            },
        }
    }

    /// Show `image` as the cursor with its hotspot at `pointer`, handing back
    /// the image it replaces so its buffer can be drawn into again.
    pub(crate) fn set_cursor(&mut self, image: CursorImage, pointer: Point) -> Option<CursorImage> {
        self.pointer = pointer;
        self.changed.cursor = true;
        self.cursor
            .replace(PlacedCursor::new(image, pointer))
            .map(PlacedCursor::into_image)
    }

    /// Move the pointer to `pointer`, answering whether a cursor is installed
    /// to follow it. The halo follows it either way.
    pub(crate) fn move_to(&mut self, pointer: Point) -> bool {
        self.pointer = pointer;
        let Some(cursor) = &mut self.cursor else {
            return false;
        };
        cursor.set_pointer(pointer);
        true
    }

    /// Withhold the overlay from the screen or show it again, answering
    /// whether that changed anything.
    pub(crate) fn set_hidden(&mut self, hidden: bool) -> bool {
        let changed = self.hidden != hidden;
        self.hidden = hidden;
        changed
    }

    pub(crate) const fn hidden(&self) -> bool {
        self.hidden
    }

    pub(crate) const fn has_cursor(&self) -> bool {
        self.cursor.is_some()
    }

    /// The screen rectangle the cursor covers, if it is shown.
    pub(crate) fn cursor_bounds(&self) -> Option<Rect> {
        self.shown_cursor().map(PlacedCursor::bounds)
    }

    fn shown_cursor(&self) -> Option<&PlacedCursor> {
        self.cursor.as_ref().filter(|_| !self.hidden)
    }

    /// Draw `ghosts` as the trail, oldest first, answering whether that
    /// changed it. Past [`MAX_GHOSTS`] the newest are kept.
    pub(crate) fn set_trail(&mut self, ghosts: &[Ghost]) -> bool {
        let newest = ghosts
            .get(ghosts.len().saturating_sub(MAX_GHOSTS)..)
            .unwrap_or(&[]);
        let mut drawn = ArrayVec::new();
        for ghost in newest.iter().filter(|ghost| ghost.opacity > 0) {
            let _ = drawn.try_push(*ghost);
        }
        if self.trail == drawn {
            return false;
        }
        self.trail = drawn;
        self.changed.trail = true;
        true
    }

    /// Draw `halo` around the pointer, answering whether that changed it.
    ///
    /// Fails closed to no halo at all when its drawing cannot be had.
    pub(crate) fn set_halo(&mut self, halo: &Halo) -> bool {
        if self.halo == *halo {
            return false;
        }
        self.changed.halo = true;
        if halo.rings().is_empty() {
            self.halo = Halo::new();
            self.art = None;
            return true;
        }
        let side = halo.reach().saturating_mul(2);
        let art = match self.art.take() {
            Some(mut held) if held.width() >= side => {
                erase(&mut held, &self.halo);
                Some(held)
            }
            _ => Surface::new(side, side),
        };
        let Some(mut art) = art else {
            self.halo = Halo::new();
            return true;
        };
        let centre = art.width() / 2;
        for ring in halo.rings() {
            let corner = centre.saturating_sub(ring.radius);
            let across = ring.radius.saturating_mul(2);
            art.wash_ring(
                corner,
                corner,
                across,
                across,
                Ring::uniform(ring.radius, ring.width),
                RingInk::Solid(ring.color),
            );
        }
        self.art = Some(art);
        self.halo = halo.clone();
        true
    }

    /// Everything the overlay draws, bottom to top: the trail oldest first,
    /// the halo, then the cursor. Nothing while hidden.
    pub(crate) fn sprites(&self) -> ArrayVec<Sprite<'_>, MAX_SPRITES> {
        let mut sprites = ArrayVec::new();
        if self.hidden {
            return sprites;
        }
        if let Some(cursor) = &self.cursor {
            for ghost in &self.trail {
                let _ = sprites.try_push(Sprite {
                    surface: cursor.image().surface(),
                    bounds: cursor.bounds_at(ghost.at),
                    opacity: ghost.opacity,
                });
            }
        }
        if let Some(art) = self.shown_art() {
            let _ = sprites.try_push(Sprite {
                surface: art,
                bounds: self.art_bounds(art),
                opacity: u8::MAX,
            });
        }
        if let Some(cursor) = &self.cursor {
            let _ = sprites.try_push(Sprite {
                surface: cursor.image().surface(),
                bounds: cursor.bounds(),
                opacity: u8::MAX,
            });
        }
        sprites
    }

    /// What of [`sprites`](Self::sprites) reaches `area`, bottom to top.
    pub(crate) fn sprites_over(&self, area: Rect) -> ArrayVec<Sprite<'_>, MAX_SPRITES> {
        let mut sprites = self.sprites();
        sprites.retain(|sprite| !sprite.bounds().intersection(&area).is_empty());
        sprites
    }

    fn shown_art(&self) -> Option<&Surface> {
        self.art.as_ref().filter(|_| !self.halo.rings().is_empty())
    }

    /// Where the halo's drawing lies: centred on the pointer.
    fn art_bounds(&self, art: &Surface) -> Rect {
        let half = i32::try_from(art.width() / 2).unwrap_or(i32::MAX);
        Rect::new(
            self.pointer.x.saturating_sub(half),
            self.pointer.y.saturating_sub(half),
            art.width(),
            art.height(),
        )
    }

    /// Where each part would lie if the overlay were composed now.
    fn footprint(&self) -> Footprint {
        let mut now = Footprint::default();
        if self.hidden {
            return now;
        }
        if let Some(cursor) = &self.cursor {
            now.cursor = Some(cursor.bounds());
            for ghost in &self.trail {
                let _ = now.trail.try_push(cursor.bounds_at(ghost.at));
            }
        }
        if self.shown_art().is_some() {
            halo_cover(self.pointer, self.halo.rings(), &mut now.halo);
        }
        now
    }

    /// Hand `mark` every rectangle whose pixels differ between the last
    /// composite and the overlay drawn as `now`: each part that changed or
    /// moved, where it was and where it is.
    fn owed(&self, now: &Footprint, mut mark: impl FnMut(Rect)) {
        let Changed {
            cursor,
            trail,
            halo,
        } = self.changed;
        let drawn = &self.drawn;
        if cursor || drawn.cursor != now.cursor {
            drawn
                .cursor
                .iter()
                .chain(&now.cursor)
                .copied()
                .for_each(&mut mark);
        }
        if trail || drawn.trail != now.trail {
            drawn
                .trail
                .iter()
                .chain(&now.trail)
                .copied()
                .for_each(&mut mark);
        }
        if halo || drawn.halo != now.halo {
            drawn
                .halo
                .iter()
                .chain(&now.halo)
                .copied()
                .for_each(&mut mark);
        }
    }

    /// Whether the next composite owes the overlay a pixel of `screen`.
    pub(crate) fn has_damage(&self, screen: Rect) -> bool {
        let mut owed = false;
        self.owed(&self.footprint(), |rect| {
            owed |= !rect.intersection(&screen).is_empty();
        });
        owed
    }

    /// Hand `mark` every rectangle the overlay's pixels changed in since the
    /// last composite, and record where each part now lies.
    pub(crate) fn settle(&mut self, mark: impl FnMut(Rect)) {
        let now = self.footprint();
        self.owed(&now, mark);
        self.drawn = now;
        self.changed = Changed::default();
    }

    /// Every rectangle the overlay drew at the last composite.
    #[cfg(test)]
    pub(crate) fn drawn(&self) -> impl Iterator<Item = Rect> + '_ {
        self.drawn.rects()
    }
}

/// Clear what `drawn` put on `art`, a square it was drawn centred in: only
/// the bands its rings cover, since everything else is already clear.
fn erase(art: &mut Surface, drawn: &Halo) {
    let half = i32::try_from(art.width() / 2).unwrap_or(i32::MAX);
    let mut bands = ArrayVec::new();
    halo_cover(Point::new(half, half), drawn.rings(), &mut bands);
    for band in bands {
        let (Ok(x), Ok(top)) = (u32::try_from(band.left()), u32::try_from(band.top())) else {
            continue;
        };
        for y in top..top.saturating_add(band.height) {
            if let Some((_, row)) = art.row_span_mut(y, x, band.width) {
                row.fill(Pixel::TRANSPARENT);
            }
        }
    }
}

/// Every pixel `rings` can touch around the pixel corner `centre`, as
/// rectangles: overlapping bands share one annulus, and each annulus is cut
/// into slabs whose hole is left out.
fn halo_cover(centre: Point, rings: &[HaloRing], out: &mut ArrayVec<Rect, MAX_HALO_COVER>) {
    let mut bands: ArrayVec<(u32, u32), MAX_HALO_RINGS> = ArrayVec::new();
    for ring in rings {
        let _ = bands.try_push((ring.inner(), ring.radius));
    }
    bands.as_mut_slice().sort_unstable();
    let mut merged: ArrayVec<(u32, u32), MAX_HALO_RINGS> = ArrayVec::new();
    for (inner, outer) in bands {
        match merged.as_mut_slice().last_mut() {
            // A pixel of anti-aliasing either side can join two bands that
            // just miss each other.
            Some(last) if inner <= last.1.saturating_add(1) => last.1 = last.1.max(outer),
            _ => {
                let _ = merged.try_push((inner, outer));
            }
        }
    }
    for (inner, outer) in merged {
        annulus_cover(centre, inner, outer, out);
    }
}

/// The rectangles covering every pixel a band from `inner` to `outer` pixels
/// around the pixel corner `centre` can touch, in [`RING_SLABS`] slabs.
///
/// A slab reaches as far out as the outer edge does at its row nearest the
/// centre, and leaves out as much of the hole as lies inside the inner edge at
/// its row farthest from it, less a pixel for the edge's anti-aliasing.
fn annulus_cover(centre: Point, inner: u32, outer: u32, out: &mut ArrayVec<Rect, MAX_HALO_COVER>) {
    if outer == 0 {
        return;
    }
    let slab = i64::from(outer.saturating_mul(2).div_ceil(RING_SLABS).max(1));
    let (outer, inner) = (i64::from(outer), i64::from(inner));
    let half_width = |radius: i64, dy: i64| -> i64 {
        let squared = u64::try_from(radius * radius - dy * dy).unwrap_or(0);
        i64::try_from(squared.isqrt()).unwrap_or(0)
    };
    let (cx, cy) = (i64::from(centre.x), i64::from(centre.y));
    let mut top = -outer;
    while top < outer {
        let bottom = (top + slab).min(outer);
        let near = if top <= 0 && bottom >= 0 {
            0
        } else {
            top.abs().min(bottom.abs())
        };
        let far = top.abs().max(bottom.abs());
        let reach = {
            let floor = half_width(outer, near);
            if floor * floor < outer * outer - near * near {
                floor + 1
            } else {
                floor
            }
        };
        let hole = if far < inner {
            (half_width(inner, far) - 1).max(0)
        } else {
            0
        };
        let rect = |left: i64, right: i64| -> Option<Rect> {
            let x = i32::try_from(cx + left).ok()?;
            let y = i32::try_from(cy + top).ok()?;
            let width = u32::try_from(right - left).ok()?;
            let height = u32::try_from(bottom - top).ok()?;
            Some(Rect::new(x, y, width, height))
        };
        let spans = if hole == 0 {
            [rect(-reach, reach), None]
        } else {
            [rect(-reach, -hole), rect(hole, reach)]
        };
        for span in spans.into_iter().flatten().filter(|rect| !rect.is_empty()) {
            let _ = out.try_push(span);
        }
        top = bottom;
    }
}

#[cfg(test)]
#[path = "pointer_tests.rs"]
mod tests;

//! Screen capture: the scene composed afresh for the desktop session
//! (`plans/CAPTURE.md` CP5).
//!
//! No memory holds the screen as it is seen — windows on layers are blended
//! during scan-out and the pointer rides the cursor plane — and nothing reads
//! scan-out back, so a capture composes the scene through the one composite
//! path into a surface the session owns. Each frost is blurred afresh from what
//! the capture shows beneath it, since a retained field holds what the capture
//! leaves out.

use alloc::vec::Vec;

use tairix_inline::ArrayVec;
use tairix_raster::Blurring;

use super::{covers, shape_coverage, ChromeFallback, Compositor, Pass, Target};
use crate::color::{Color, Pixel};
use crate::frost::surface_span;
use crate::geometry::{Rect, Region};
use crate::pointer::{PointerOverlay, MAX_SPRITES};
use crate::surface::Surface;
use crate::window::{Window, WindowId};

/// What a capture shows in place of a window it blanks, whatever the theme.
const BLANKED: Color = Color::rgb(0, 0, 0);

/// What a screen capture shows, and of which part of the desktop.
///
/// Armed by the desktop session ([`Compositor::arm_capture`]); no window,
/// display or protocol request reaches one.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CaptureTap {
    /// The desktop rectangle captured, in desktop pixels.
    pub area: Rect,
    /// The one window shown, with the transients it owns, every other left
    /// out as though absent; every window is shown where this is `None`.
    pub only: Option<WindowId>,
    /// Windows left out as though absent, with the transients they own: the
    /// capture's own chrome.
    pub exclude: Vec<WindowId>,
    /// Windows shown as opaque black over their bounds, with the transients
    /// they own: the lock and the authorisation sheet. A window both left out
    /// and blanked is blanked.
    pub blank: Vec<WindowId>,
    /// Whether the pointer overlay — the cursor, its trail and its halo — is
    /// drawn.
    pub pointer: bool,
}

impl CaptureTap {
    /// Whether the capture draws anything of `window`.
    fn draws(&self, window: &Window) -> bool {
        self.blanks(window) || (self.shows(window) && !listed(&self.exclude, window))
    }

    /// Whether `window` is of what the tap shows: every window, or the one it
    /// is armed for and that one's transients.
    fn shows(&self, window: &Window) -> bool {
        self.only.is_none_or(|only| listed(&[only], window))
    }

    /// Whether the capture shows `window` as opaque black over its bounds.
    fn blanks(&self, window: &Window) -> bool {
        listed(&self.blank, window)
    }
}

/// Whether `window`, or the window it is a transient of, is one of `ids`.
fn listed(ids: &[WindowId], window: &Window) -> bool {
    ids.contains(&window.id()) || window.parent().is_some_and(|owner| ids.contains(&owner))
}

/// Why a capture could not be armed or composed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CaptureError {
    /// The area is empty or not wholly on the desktop.
    Area,
    /// No tap is armed: none was, or the desktop changed size under it.
    Disarmed,
    /// The surface is not the size of the tap's area.
    Surface,
    /// Composing needed memory the heap or the reserved blur scratch refused.
    OutOfMemory,
}

/// An armed tap, and what of its area the scene changed since its changes
/// were last taken.
pub(super) struct Armed {
    tap: CaptureTap,
    changed: Region,
    /// A region routed to the tap clipped to its area, kept for its buffer.
    clipped: Region,
    pointer: PointerSeen,
}

impl Armed {
    /// Owe the tap what of `rect` lies in its area.
    fn owe(&mut self, rect: Rect) {
        self.changed.add(rect.intersection(&self.tap.area));
    }

    /// Owe the tap where the pointer overlay's sprites were and are: each one
    /// that moved, or all of them where the overlay changed what it draws.
    fn follow(&mut self, overlay: &PointerOverlay) {
        let now = PointerSeen::of(overlay);
        let seen = core::mem::take(&mut self.pointer);
        if seen.shape == now.shape && seen.bounds.len() == now.bounds.len() {
            for (was, is) in seen.bounds.iter().zip(&now.bounds) {
                if was != is {
                    self.owe(*was);
                    self.owe(*is);
                }
            }
        } else {
            for rect in seen.bounds.iter().chain(&now.bounds) {
                self.owe(*rect);
            }
        }
        self.pointer = now;
    }
}

/// The pointer overlay as a tap last saw it: what it drew, and where each
/// sprite lay.
#[derive(Default)]
struct PointerSeen {
    shape: u64,
    bounds: ArrayVec<Rect, MAX_SPRITES>,
}

impl PointerSeen {
    fn of(overlay: &PointerOverlay) -> Self {
        let mut bounds = ArrayVec::new();
        for sprite in &overlay.sprites() {
            let _ = bounds.try_push(sprite.bounds());
        }
        Self {
            shape: overlay.shape(),
            bounds,
        }
    }
}

/// What every pass of one capture reads.
struct Capturing<'c> {
    tap: &'c CaptureTap,
    /// The root fill, premultiplied.
    base: Pixel,
    /// Furniture the cache does not hold, rendered for this capture alone.
    fallback: &'c ChromeFallback,
}

/// What a capture composes into: the session's picture, or the frost plane a
/// capture's frost reads its backdrop from.
enum Canvas<'s> {
    Picture(&'s mut Surface),
    Backdrop,
}

impl Canvas<'_> {
    /// This canvas as the target of one more pass.
    fn target(&mut self) -> Target<'_, 'static> {
        match self {
            Self::Picture(picture) => Target::Capture(picture),
            Self::Backdrop => Target::Plane,
        }
    }
}

impl Compositor {
    /// Arm `tap`, replacing any tap armed before, and owe it its whole area.
    ///
    /// From then on every change the scene makes inside the area reaches the
    /// tap however the display shows it: composited, on a layer of its own,
    /// or the pointer on the cursor plane. A tap lasts until it is disarmed or
    /// the desktop changes size.
    ///
    /// # Errors
    ///
    /// [`CaptureError::Area`], arming nothing, for an area that is empty or
    /// not wholly on the desktop.
    pub fn arm_capture(&mut self, tap: CaptureTap) -> Result<(), CaptureError> {
        let area = tap.area;
        if area.is_empty() || area.intersection(&self.screen_rect()) != area {
            return Err(CaptureError::Area);
        }
        self.capture = Some(Armed {
            tap,
            changed: Region::from(area),
            clipped: Region::new(),
            pointer: PointerSeen::of(&self.pointer),
        });
        Ok(())
    }

    /// Disarm the tap, forgetting what it was owed, answering whether one was
    /// armed. Nothing on screen changes.
    pub fn disarm_capture(&mut self) -> bool {
        self.capture.take().is_some()
    }

    /// Hand over, into `changed`, what of the armed tap's area changed since
    /// this was last asked, in desktop pixels: the pointer overlay's own
    /// movement included where the tap shows it. `false`, `changed` left
    /// empty, while no tap is armed.
    ///
    /// The two regions trade places, so neither allocates once both have
    /// grown.
    pub fn take_capture_changes(&mut self, changed: &mut Region) -> bool {
        changed.clear();
        let Self {
            capture, pointer, ..
        } = self;
        let Some(armed) = capture.as_mut() else {
            return false;
        };
        if armed.tap.pointer {
            armed.follow(pointer);
        }
        core::mem::swap(changed, &mut armed.changed);
        true
    }

    /// Compose `region` of the armed tap's area into `into`, a surface of the
    /// area's size whose first pixel is the area's top-left.
    ///
    /// The scene is composed afresh, in bands across the job runner: at full
    /// strength whatever the screen reveal, at desktop resolution, the windows
    /// the tap leaves out absent, those it blanks opaque black over their
    /// bounds, and the pointer overlay drawn from its own images where the tap
    /// shows it. It is the same picture whether the frame was composited,
    /// layered, promoted or scaled, and every pixel written is opaque, so its
    /// premultiplied pixels are the straight-alpha ones a PNG encoder reads.
    /// The live frame is untouched — its damage, what the display is owed, its
    /// counters, the retained frosts and the furniture cache — and so are the
    /// pixels of `into` outside `region`.
    ///
    /// # Errors
    ///
    /// [`CaptureError::Disarmed`] with no tap armed and
    /// [`CaptureError::Surface`] for a surface not the area's size, both
    /// writing nothing; [`CaptureError::OutOfMemory`] where composing needed
    /// memory that was refused, leaving `into` partly composed.
    pub fn compose_capture(
        &mut self,
        into: &mut Surface,
        region: &Region,
    ) -> Result<(), CaptureError> {
        let Some(armed) = self.capture.take() else {
            return Err(CaptureError::Disarmed);
        };
        let composed = self.compose_tapped(&armed.tap, into, region);
        self.capture = Some(armed);
        composed
    }

    /// Owe the armed tap `rect` of the scene, which changed.
    pub(super) fn tap_changed(&mut self, rect: Rect) {
        if let Some(armed) = self.capture.as_mut() {
            armed.owe(rect);
        }
    }

    /// [`tap_changed`](Self::tap_changed) for every pixel of `region`, in one
    /// merge.
    pub(super) fn tap_changed_region(&mut self, region: &Region) {
        if let Some(armed) = self.capture.as_mut() {
            armed.clipped.clone_from(region);
            armed.clipped.clip(armed.tap.area);
            armed.changed.add_region(&armed.clipped);
        }
    }

    /// [`compose_capture`](Self::compose_capture) of `tap`, held out of the
    /// compositor while its passes borrow it.
    fn compose_tapped(
        &mut self,
        tap: &CaptureTap,
        into: &mut Surface,
        region: &Region,
    ) -> Result<(), CaptureError> {
        let area = tap.area;
        if (into.width(), into.height()) != (area.width, area.height) {
            return Err(CaptureError::Surface);
        }
        let Some((x, y)) = area.surface_origin() else {
            return Err(CaptureError::Area);
        };
        self.ensure_shadow_tiles();
        let reach = self.frost_reach(|window| window.is_visible() && window.blur_radius() > 0);
        let drawn = |window: &Window| {
            tap.draws(window)
                && !tap.blanks(window)
                && region
                    .rects()
                    .iter()
                    .any(|&rect| covers(window, rect.intersection(&area).outset(reach)))
        };
        let mut fallback = ChromeFallback::new();
        self.render_unresident(&drawn, &mut fallback);
        let shot = Capturing {
            tap,
            base: self.background.premultiply(),
            fallback: &fallback,
        };
        let mut hits = core::mem::take(&mut self.hits);
        let mut composed = true;
        into.with_origin(x, y, |picture| {
            for &rect in region.rects() {
                let rect = rect.intersection(&area);
                if rect.is_empty() {
                    continue;
                }
                self.gather_hits(
                    rect,
                    self.windows.len(),
                    |window| tap.draws(window),
                    &mut hits,
                );
                composed &= self.capture_rect(&shot, picture, rect, &hits);
            }
        });
        self.hits = hits;
        if composed {
            Ok(())
        } else {
            Err(CaptureError::OutOfMemory)
        }
    }

    /// Compose `rect` of the capture into `picture` from the windows `hits`
    /// names, each frosted one over a fresh blur of what the capture shows
    /// beneath it, answering whether every row was composed.
    fn capture_rect(
        &mut self,
        shot: &Capturing<'_>,
        picture: &mut Surface,
        rect: Rect,
        hits: &[usize],
    ) -> bool {
        let (mut under, mut start, mut composed) = (Some(shot.base), 0, true);
        for (split, &index) in hits.iter().enumerate() {
            let frosted = self.windows.get(index).is_some_and(|window| {
                window.blur_radius() > 0
                    && !shot.tap.blanks(window)
                    && !window.bounds().intersection(&rect).is_empty()
            });
            if !frosted {
                continue;
            }
            composed &= self.compose_blanked(
                shot,
                (rect, under),
                hits.get(start..split).unwrap_or_default(),
                Pass::Capture { pointer: false },
                Canvas::Picture(&mut *picture),
            );
            composed &= self.capture_frost(shot, index, rect, picture);
            start = split;
            under = None;
        }
        composed &= self.compose_blanked(
            shot,
            (rect, under),
            hits.get(start..).unwrap_or_default(),
            Pass::Capture {
                pointer: shot.tap.pointer,
            },
            Canvas::Picture(picture),
        );
        composed
    }

    /// Compose the windows `span` names over `area` into `canvas` from
    /// `under` on `pass`, each one the tap blanks an opaque fill over its
    /// bounds in place of its pixels and its shadow, answering whether every
    /// row was composed.
    fn compose_blanked(
        &mut self,
        shot: &Capturing<'_>,
        (area, mut under): (Rect, Option<Pixel>),
        span: &[usize],
        pass: Pass,
        mut canvas: Canvas<'_>,
    ) -> bool {
        let (mut start, mut composed) = (0, true);
        for (at, &index) in span.iter().enumerate() {
            let Some(bounds) = self
                .windows
                .get(index)
                .filter(|window| shot.tap.blanks(window))
                .map(Window::bounds)
            else {
                continue;
            };
            composed &= self.compose_span(
                area,
                under,
                span.get(start..at),
                shot.fallback,
                Pass::Capture { pointer: false },
                canvas.target(),
            );
            self.blank(&mut canvas, bounds.intersection(&area));
            start = at.saturating_add(1);
            under = None;
        }
        composed &= self.compose_span(
            area,
            under,
            span.get(start..),
            shot.fallback,
            pass,
            canvas.target(),
        );
        composed
    }

    /// Fill `rect` of `canvas` with what a capture shows of a window it
    /// blanks.
    fn blank(&mut self, canvas: &mut Canvas<'_>, rect: Rect) {
        let Some((x, y)) = rect.surface_origin() else {
            return;
        };
        match canvas {
            Canvas::Picture(picture) => picture.fill_rect(x, y, rect.width, rect.height, BLANKED),
            Canvas::Backdrop => self
                .frost_plane
                .fill_rect(x, y, rect.width, rect.height, BLANKED),
        }
    }

    /// Frost the window at z-index `index` over `rect` of `picture`: what the
    /// capture shows beneath it, blurred afresh and mixed in at its shape's
    /// coverage, answering whether the frost was written.
    fn capture_frost(
        &mut self,
        shot: &Capturing<'_>,
        index: usize,
        rect: Rect,
        picture: &mut Surface,
    ) -> bool {
        let screen = self.screen_rect();
        let Some(window) = self.windows.get(index) else {
            return true;
        };
        let (bounds, shape) = (window.bounds(), window.shape());
        let (radius, reach) = (self.blur_radius_px(window), self.frost_reach_px(window));
        let shown = bounds.intersection(&rect);
        let Some(span) = surface_span(shown) else {
            return true;
        };
        if !self.capture_backdrop(shot, index, shown.outset(reach).intersection(&screen)) {
            return false;
        }
        let bands = [span];
        let blurring = Blurring {
            rect: (0, 0, screen.width, screen.height),
            bands: &bands,
            radius,
        };
        let runner = self.runner;
        picture.frost_halved_from(
            &mut self.frost_plane,
            &blurring,
            &mut self.blur_scratch,
            runner,
            |x, y| shape_coverage(shape, bounds, x, y),
        )
    }

    /// Compose what the capture shows beneath the window at z-index `index`
    /// over `area` into the frost plane, every window there unfrosted: the
    /// backdrop its frost blurs. Answers whether every row was composed.
    fn capture_backdrop(&mut self, shot: &Capturing<'_>, index: usize, area: Rect) -> bool {
        let mut hits = core::mem::take(&mut self.backdrop_hits);
        self.gather_hits(area, index, |window| shot.tap.draws(window), &mut hits);
        let composed = self.compose_blanked(
            shot,
            (area, Some(shot.base)),
            &hits,
            Pass::Capture { pointer: false },
            Canvas::Backdrop,
        );
        self.backdrop_hits = hits;
        composed
    }
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;

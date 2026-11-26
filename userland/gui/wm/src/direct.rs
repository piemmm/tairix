//! A promoted window presented straight from its client's frame
//! (`plans/GPU.md` GP6.4).
//!
//! While a window is the screen alone, its presents are laid straight into
//! the buffer the display scans — the layer it is promoted to, or the
//! display's own frame — rather than decoded into its content and composed
//! out of it again. The compositor holds the frame the window last showed, and
//! the content is owed what that frame changed: brought up to date before
//! anything composes, bakes, captures or pictures it. The back buffer is owed
//! what a frame laid into the display's own frame showed, recomposed once
//! anything else can be seen.

use tairix_abi::driver::display::{DamageList, DisplayMode};
use tairix_abi::{DriverError, Errno};
use tairix_display::{winframe, ChannelOrder, Presenter, SharedFrame};
use tairix_parallel::JobRunner;

use super::{Compositor, Presentation};
use crate::geometry::{Rect, Region};
use crate::pointer::PointerRoute;
use crate::window::{Window, WindowId};

/// The promoted window whose presents reach the display straight from its
/// client's frame: the frame it last showed, that frame's shape, and what of
/// it the window's content lacks, in the window's own pixels.
pub(super) struct Direct {
    id: WindowId,
    frame: SharedFrame,
    mode: DisplayMode,
    owed: Region,
}

impl Direct {
    /// The window presented straight.
    pub(super) const fn id(&self) -> WindowId {
        self.id
    }

    /// Lay `region` of the frame, in the window's own pixels, into `target`,
    /// answering whether every pixel laid is opaque.
    pub(super) fn lay(
        &self,
        region: &Region,
        target: (&mut [u8], usize, ChannelOrder),
        runner: &dyn JobRunner,
    ) -> Result<bool, Errno> {
        winframe::lay_judged_into(self.frame.bytes(), &self.mode, target, region, runner)
    }
}

impl Compositor {
    /// Whether window `id`'s next present may be laid straight from its
    /// client's frame rather than decoded: the frame on the display is `id`
    /// alone, promoted, and it may be still — the screen's opaque fullscreen
    /// cover, carrying no view, with no capture armed.
    #[must_use]
    pub fn presents_directly(&self, id: WindowId) -> bool {
        matches!(self.presented, Some(Presentation::Promoted(shown)) if shown == id)
            && self.capture.is_none()
            && self.direct.as_ref().is_none_or(|direct| direct.id == id)
            && self.fullscreen_cover().is_some_and(|cover| {
                cover.id() == id && cover.view().is_none() && Self::promotable(cover)
            })
    }

    /// Hold `frame`, shaped as `mode`, as window `id`'s newest and owe its
    /// content `damage` of it rather than decode it, answering whether it was
    /// owed: only where [`Self::presents_directly`] says so, the content is
    /// held at the frame's size, and every row of the frame can be laid. A
    /// present not owed is decoded as ever.
    ///
    /// The frame is the window's to show until a later present of it is
    /// owed or decoded; holding it keeps its region mapped.
    pub fn owe_window_content(
        &mut self,
        id: WindowId,
        frame: &SharedFrame,
        mode: &DisplayMode,
        damage: &DamageList,
    ) -> bool {
        if !self.presents_directly(id) || winframe::holds(mode, frame.bytes().len()).is_err() {
            return false;
        }
        let Some(window) = self.window(id) else {
            return false;
        };
        let held = window.content().is_some_and(|content| {
            (content.width(), content.height()) == (mode.width_px, mode.height_px)
        });
        if !held {
            return false;
        }
        let client = window.client_rect();
        let mut marked = core::mem::take(&mut self.present_changes);
        marked.clear();
        for rect in damage.rects() {
            marked.add(Rect::new(
                rect.x.cast_signed(),
                rect.y.cast_signed(),
                rect.width_px,
                rect.height_px,
            ));
        }
        match self.direct.as_mut() {
            Some(direct) => {
                direct.frame = frame.clone();
                direct.mode = *mode;
                direct.owed.add_region(&marked);
            }
            None => {
                self.direct = Some(Direct {
                    id,
                    frame: frame.clone(),
                    mode: *mode,
                    owed: marked.clone(),
                });
            }
        }
        marked.translate(client.left(), client.top());
        marked.clip(client);
        self.mark_layer_region(id, &marked);
        marked.clear();
        self.present_changes = marked;
        true
    }

    /// Bring the owed window's content up to the frame it last showed.
    pub(super) fn settle_content(&mut self) {
        let Some(direct) = self
            .direct
            .as_mut()
            .filter(|direct| !direct.owed.is_empty())
        else {
            return;
        };
        let mut owed = core::mem::take(&mut direct.owed);
        let id = direct.id;
        let index = self.index_of(id);
        let mut changed = core::mem::take(&mut self.present_changes);
        changed.clear();
        let Self {
            direct,
            windows,
            runner,
            ..
        } = self;
        if let (Some(direct), Some(window)) = (
            direct.as_ref(),
            index.and_then(|index| windows.get_mut(index)),
        ) {
            if let Some(content) = window.content_mut() {
                // Owed only of a frame that holds every row, at the content's
                // own size, so the decode cannot be refused.
                let _ = winframe::decode_region(
                    direct.frame.bytes(),
                    content,
                    &direct.mode,
                    &owed,
                    *runner,
                    |rect| changed.add(rect),
                );
            }
            for local in changed.rects() {
                window.content_changed(*local);
            }
        }
        changed.clear();
        self.present_changes = changed;
        owed.clear();
        if let Some(direct) = self.direct.as_mut() {
            direct.owed = owed;
        }
    }

    /// Let window `id`'s held frame go, its content brought up to date first:
    /// what reshapes or rewrites the content ends the window's presenting
    /// straight, as does its client being handed the frame back.
    pub fn end_direct(&mut self, id: WindowId) {
        if self.direct.as_ref().is_some_and(|direct| direct.id == id) {
            self.settle_content();
            self.direct = None;
        }
    }

    /// Let window `id`'s held frame go with its content, which is gone.
    pub(super) fn forget_direct(&mut self, id: WindowId) {
        if self.direct.as_ref().is_some_and(|direct| direct.id == id) {
            self.direct = None;
        }
    }

    /// Settle what a reader of window `id`'s content is about to read.
    pub(super) fn settle_window(&mut self, id: WindowId) {
        if self.direct.as_ref().is_some_and(|direct| direct.id == id) {
            self.settle_content();
        }
    }

    /// Whether the frame drawn into a display's own memory may be window `id`
    /// alone: the opaque fullscreen cover, unscaled, unfaded and uncaptured,
    /// nothing of the pointer drawn over it.
    pub(super) fn frame_shows_alone(&self, id: WindowId) -> bool {
        self.layers.is_none()
            && self.capture.is_none()
            && self.reveal == u8::MAX
            && self.scaled.is_none()
            && (self.pointer.route() != PointerRoute::Frame
                || self.pointer.sprite_bounds().is_none())
            && self.fullscreen_cover().is_some_and(|cover| {
                cover.id() == id && cover.view().is_none() && Self::promotable(cover)
            })
    }

    /// How a frame drawn into the display's own memory now reached it: the
    /// cover promoted where it shows alone, else composited.
    pub(super) fn drawn_presentation(&self) -> Presentation {
        self.fullscreen_cover()
            .map(Window::id)
            .filter(|&id| self.frame_shows_alone(id))
            .map_or(Presentation::Composited, Presentation::Promoted)
    }

    /// Present the owed window, shown alone, straight from its frame into the
    /// display's own back frame, answering whether it was. A display that
    /// draws no frame of its own, a window no longer shown alone, and a pixel
    /// laid short of opaque — the content then brought up to date, which
    /// records the translucency — each leave the frame to be composed.
    ///
    /// # Errors
    ///
    /// The display's refusal of the frame laid.
    pub(super) fn present_direct_frame(
        &mut self,
        presenter: &mut dyn Presenter,
    ) -> Result<bool, DriverError> {
        let Some(id) = self.direct.as_ref().map(Direct::id) else {
            return Ok(false);
        };
        if self.refused.drawn || !self.frame_shows_alone(id) {
            return Ok(false);
        }
        let mut changed = core::mem::take(&mut self.direct_changed);
        changed.clear();
        changed.add_region(&self.damage);
        changed.add_region(&self.undelivered);
        changed.add_region(&self.scanout);
        changed.clip(self.screen_rect());
        let mut laid = None;
        let drawn = match self.direct.as_ref() {
            Some(direct) => {
                let (order, runner) = (self.order, self.runner);
                presenter.present_drawn(&changed, &mut |bytes, stride, refresh| {
                    let stride = usize::try_from(stride).map_err(|_| DriverError::OutOfRange)?;
                    // The window covers the screen from its origin, so its
                    // pixels are the screen's.
                    let judged = direct.lay(refresh, (bytes, stride, order), runner);
                    let opaque = judged == Ok(true);
                    laid = Some(judged);
                    // Refused here, nothing of it is shown and the frame is
                    // owed it whole.
                    if opaque {
                        Ok(())
                    } else {
                        Err(DriverError::Unsupported)
                    }
                })
            }
            None => Ok(()),
        };
        let shown = match (laid, drawn) {
            (Some(Ok(true)), Ok(())) => {
                self.direct_hidden.add_region(&changed);
                self.damage.clear();
                self.undelivered.clear();
                self.scanout.clear();
                self.heap_behind = true;
                self.stats.bump_present();
                self.presented = Some(Presentation::Promoted(id));
                Ok(true)
            }
            (Some(Ok(true)), Err(err)) => Err(err),
            // Translucent, or a lay refused: composed from the content.
            (Some(_), _) => {
                self.end_direct(id);
                Ok(false)
            }
            (None, Err(DriverError::Unsupported)) => {
                self.refused.drawn = true;
                Ok(false)
            }
            (None, Err(err)) => Err(err),
            (None, Ok(())) => Ok(false),
        };
        changed.clear();
        self.direct_changed = changed;
        shown
    }

    /// Give the back buffer back what was laid straight into the display's
    /// own frame, for the next composite to recompose.
    pub(super) fn replay_direct_hidden(&mut self) {
        if self.direct_hidden.is_empty() {
            return;
        }
        let mut hidden = core::mem::take(&mut self.direct_hidden);
        self.damage.add_region(&hidden);
        hidden.clear();
        self.direct_hidden = hidden;
    }

    /// Let the held frame go once its window is no longer presented straight
    /// and owes nothing.
    pub(super) fn retire_direct(&mut self) {
        let retired = self
            .direct
            .as_ref()
            .is_some_and(|direct| direct.owed.is_empty() && !self.presents_directly(direct.id));
        if retired {
            self.direct = None;
        }
    }
}

#[cfg(test)]
#[path = "direct_tests.rs"]
mod tests;

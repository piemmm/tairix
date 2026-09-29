//! The seat's drag carrier: a file the user drags out of an application,
//! carried by the desktop to the icon-bar slot of an application that opens
//! it.
//!
//! Only the file's *name* reaches the session — what an application's
//! declared types are matched against as the pointer passes over its slot.
//! The dragging application keeps the file, and when the drag is dropped on
//! an application that claims it, it opens the file for that application
//! itself, exactly as its own "Open With" would. So no path and no authority
//! crosses to the desktop, and nothing is opened on an application's word.
//!
//! A drag is the press that began it, carried on: it holds the pointer until
//! that press comes up, and `Escape` or any other button ends it with nothing
//! dropped. The plate naming what is carried floats beside the pointer the way
//! the seat's tooltip plate does — input-transparent, above everything — so it
//! can never become the thing it is dropped on.

use alloc::string::String;

use tairix_abi::window_ipc::DropTarget;
use tairix_abi::Errno;
use tairix_controls::Tooltip;
use tairix_geometry::{Point, Rect};
use tairix_theme::Theme;
use tairix_wm::{
    Compositor, Corners, InputEvent, Key, NamedKey, PointerButton, PointerCatch, Surface, WindowId,
};

use crate::shell::DesktopShell;

/// How far the plate sits from the pointer's hotspot, right and down, in
/// *logical* pixels: clear of the cursor it follows.
const PLATE_OFFSET: u32 = 14;

/// How a carried drag ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DragEnd {
    /// The window-channel id of the window the drag began in.
    pub source: u64,
    /// The application it was dropped on, or `None` for a drop anywhere else.
    pub target: Option<DropTarget>,
}

/// The one drag the seat carries, when there is one.
#[derive(Debug, Default)]
pub(crate) struct DragCarrier {
    carried: Option<Carried>,
}

#[derive(Debug)]
struct Carried {
    source: u64,
    name: String,
    /// The compositor window of the plate naming what is carried.
    plate: Option<WindowId>,
    /// The application slot under the pointer, if one is.
    slot: Option<usize>,
    /// What that slot does with the file, if it takes it.
    target: Option<DropTarget>,
}

impl DesktopShell {
    /// Whether a drag is being carried.
    #[must_use]
    pub const fn drag_active(&self) -> bool {
        self.drag.carried.is_some()
    }

    /// Begin carrying the file `name` out of the window-channel window
    /// `source`, whose compositor window `wm` holds the press that began it.
    ///
    /// # Errors
    ///
    /// * [`Errno::AlreadyExists`] — a drag is already being carried.
    /// * [`Errno::PermissionDenied`] — no press is held in `wm`: a drag is a
    ///   press carried on, so one cannot begin anywhere else.
    pub fn begin_drag(
        &mut self,
        compositor: &mut Compositor,
        source: u64,
        wm: WindowId,
        name: &str,
    ) -> Result<(), Errno> {
        if self.drag_active() {
            return Err(Errno::AlreadyExists);
        }
        if self.router().pressed_in() != Some(wm) {
            return Err(Errno::PermissionDenied);
        }
        self.yield_pointer(compositor);
        let at = self.router().pointer();
        let plate = place_plate(None, name, at, self.session().floating_theme(), compositor);
        self.drag.carried = Some(Carried {
            source,
            name: String::from(name),
            plate,
            slot: None,
            target: None,
        });
        Ok(())
    }

    /// Carry the drag through one pointer `event`, asking `accepts` what an
    /// application slot does with the file as the pointer arrives on it.
    ///
    /// Answers how the drag ended, once it has: the press coming up drops it
    /// where it is, and any other press ends it with nothing dropped. What it
    /// changes on the bar is latched for the caller's settle to paint.
    pub fn drag_pointer(
        &mut self,
        compositor: &mut Compositor,
        event: &InputEvent,
        accepts: &mut dyn FnMut(usize, &str) -> Option<DropTarget>,
    ) -> Option<DragEnd> {
        let carried = self.drag.carried.as_ref()?;
        match *event {
            InputEvent::PointerMoved { to } => {
                let (plate, slot) = (carried.plate, carried.slot);
                self.track_pointer(to, compositor);
                let theme = self.session().floating_theme();
                let name = self.drag.carried.as_ref()?.name.as_str();
                let plate = place_plate(plate, name, to, theme, compositor);
                let scale = compositor.scale();
                let under = self.session().taskbar().app_slot_at(to, scale);
                let target = if under == slot {
                    self.drag.carried.as_ref()?.target
                } else {
                    under.and_then(|slot| accepts(slot, name))
                };
                let carried = self.drag.carried.as_mut()?;
                carried.plate = plate;
                if under != slot {
                    carried.slot = under;
                    carried.target = target;
                    self.session_mut()
                        .taskbar_mut()
                        .set_drop_slot(target.and(under), scale);
                }
                None
            }
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            } => self.end_drag(compositor, true),
            InputEvent::PointerPressed { .. } => self.end_drag(compositor, false),
            _ => None,
        }
    }

    /// Carry the drag through one key `event`: `Escape` ends it with nothing
    /// dropped, and every other key is the drag's to swallow.
    pub fn drag_key(&mut self, compositor: &mut Compositor, event: &InputEvent) -> Option<DragEnd> {
        match event {
            InputEvent::KeyPressed {
                key: Key::Named(NamedKey::Escape),
                ..
            } => self.end_drag(compositor, false),
            _ => None,
        }
    }

    /// Forget which slot a carried drag is over, because the strip was
    /// replaced: an index may now name another application, so the slot is
    /// asked afresh when the pointer next moves, and lit only if it takes the
    /// file.
    pub(crate) fn forget_drop_slot(&mut self, scale: tairix_geometry::Scale) {
        let Some(carried) = self.drag.carried.as_mut() else {
            return;
        };
        carried.slot = None;
        carried.target = None;
        self.session_mut().taskbar_mut().set_drop_slot(None, scale);
    }

    /// End a drag whose source window `window` has gone, with nothing to tell
    /// it. A drag from any other window is left carried.
    pub fn abort_drag_for(&mut self, compositor: &mut Compositor, window: u64) {
        if self
            .drag
            .carried
            .as_ref()
            .is_some_and(|carried| carried.source == window)
        {
            let _ = self.end_drag(compositor, false);
        }
    }

    /// Stop carrying, answering how it ended: on the slot under the pointer
    /// if `dropped` and that slot takes the file, otherwise on nothing. The
    /// slot it lit is latched dark for the caller's settle to paint.
    pub fn end_drag(&mut self, compositor: &mut Compositor, dropped: bool) -> Option<DragEnd> {
        let carried = self.drag.carried.take()?;
        if let Some(plate) = carried.plate {
            compositor.remove(plate);
        }
        let scale = compositor.scale();
        self.session_mut().taskbar_mut().set_drop_slot(None, scale);
        Some(DragEnd {
            source: carried.source,
            target: carried.target.filter(|_| dropped),
        })
    }
}

/// Show the plate naming `name` beside the pointer at `at`: `plate` moved
/// when it is still up, since the name never changes, and painted anew when it
/// is not. Answers the plate, or `None` when the heap will not give one — the
/// drag is still carried, only unlabelled.
fn place_plate(
    plate: Option<WindowId>,
    name: &str,
    at: Point,
    theme: &Theme,
    compositor: &mut Compositor,
) -> Option<WindowId> {
    let scale = compositor.scale();
    let offset = i32::try_from(scale.scale_length(PLATE_OFFSET)).unwrap_or(i32::MAX);
    let origin = Point::new(at.x.saturating_add(offset), at.y.saturating_add(offset));
    if let Some(plate) = plate.filter(|plate| compositor.window(*plate).is_some()) {
        compositor.move_window(plate, origin);
        return Some(plate);
    }
    let label = Tooltip::new(name);
    let (width, height) = label.preferred_size(scale, theme);
    let mut pixels = Surface::new(width, height)?;
    label.render(&mut pixels, Rect::new(0, 0, width, height), scale, theme);
    let id = compositor.add_window(origin, pixels);
    compositor.set_pointer_catch(id, PointerCatch::None);
    let radius = scale.scale_length(theme.metrics().popup_corner_radius);
    compositor.set_corners(id, Corners::painted(radius));
    compositor.set_casts_shadow(id, true);
    compositor.raise(id);
    Some(id)
}

#[cfg(test)]
#[path = "drag_tests.rs"]
mod tests;

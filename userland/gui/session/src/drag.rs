//! The seat's drag carrier: items the user drags out of an application,
//! carried by the desktop to where they are dropped.
//!
//! Only the first item's name and the count reach the session — what the
//! plate says, and what an application slot's declared types are matched
//! against. Over one of the dragging application's own windows, or over the
//! desktop, the session reports where the drag is and shows on the pointer
//! the verdict the application answers: copy, move, or nothing. The
//! application keeps the items and performs every drop itself, so no path of
//! its own and no authority crosses to the desktop; the only path that
//! crosses is the desktop's own folder, which the session names and the
//! application already reaches.
//!
//! A drag is the press that began it, carried on: it holds the pointer until
//! that press comes up, and `Escape` or any other button ends it with nothing
//! dropped. The plate naming what is carried floats beside the pointer the way
//! the seat's tooltip plate does — input-transparent, above everything — so it
//! can never become the thing it is dropped on.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;

use tairix_abi::window_ipc::{DragItems, DropOperation, DropTarget};
use tairix_abi::Errno;
use tairix_controls::Tooltip;
use tairix_geometry::{Point, Rect};
use tairix_theme::{CursorKind, Theme};
use tairix_window::DragConclusion;
use tairix_wm::{
    Compositor, Corners, InputEvent, Key, NamedKey, PointerButton, PointerCatch, Surface, WindowId,
};

use crate::shell::DesktopShell;

/// How far the plate sits from the pointer's hotspot, right and down, in
/// *logical* pixels: clear of the cursor it follows.
const PLATE_OFFSET: u32 = 14;

/// How a carried drag ended: the window it began in, and where it was dropped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DragEnd {
    /// The window-channel id of the window the drag began in.
    pub source: u64,
    /// Where it was dropped.
    pub ended: DragConclusion,
}

/// What the pointer is over while a drag is carried, as the session resolves
/// it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DragSurface {
    /// Nothing that takes the drag.
    Nothing,
    /// The application slot at `index` on the icon bar, and what it does with
    /// the one file dragged, if it takes it.
    Slot {
        /// The slot's index in the strip.
        index: usize,
        /// What the slot's application does with the file. Boxed: a target
        /// carries a whole bundle path, wider than every other surface.
        target: Option<Box<DropTarget>>,
    },
    /// One of the dragging application's own windows, at a point in it.
    Window {
        /// The window under the pointer.
        window_id: u64,
        /// Window-local x.
        x: u32,
        /// Window-local y.
        y: u32,
    },
    /// The desktop, on the folder at `folder`; `icon` is the pinboard icon
    /// standing for that folder, if one does.
    Desktop {
        /// The absolute path of the folder a drop there lands in.
        folder: String,
        /// The pinboard icon the pointer is on, when the folder is one.
        icon: Option<usize>,
        /// The revision of the desktop's listing `icon` indexes.
        revision: u64,
    },
}

/// Where a carried drag's pointer is, as an identity resolved per motion
/// without allocating: what it is over is built only when this changes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DragPlace {
    /// Nothing that takes the drag.
    Nothing,
    /// The icon-bar slot at `index`.
    Slot {
        /// The slot's index in the strip.
        index: usize,
    },
    /// One of the dragging application's own windows, at a point in it.
    Window {
        /// The window under the pointer.
        window_id: u64,
        /// Window-local x.
        x: u32,
        /// Window-local y.
        y: u32,
    },
    /// The desktop: the folder icon `icon` of its listing at `revision`, or
    /// its own folder.
    Desktop {
        /// The pinboard icon the pointer is on, when the folder is one.
        icon: Option<usize>,
        /// The revision of the listing `icon` indexes.
        revision: u64,
    },
}

impl DragSurface {
    /// The place this is what is over.
    #[must_use]
    pub const fn place(&self) -> DragPlace {
        match *self {
            Self::Nothing => DragPlace::Nothing,
            Self::Slot { index, .. } => DragPlace::Slot { index },
            Self::Window { window_id, x, y } => DragPlace::Window { window_id, x, y },
            Self::Desktop { icon, revision, .. } => DragPlace::Desktop { icon, revision },
        }
    }

    /// Whether `self` and `other` are the same place to drop, wherever on it
    /// the pointer is: a verdict for one still holds for the other.
    fn same_place(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Nothing, Self::Nothing) => true,
            (Self::Slot { index: a, .. }, Self::Slot { index: b, .. }) => a == b,
            (Self::Window { window_id: a, .. }, Self::Window { window_id: b, .. }) => a == b,
            (Self::Desktop { folder: a, .. }, Self::Desktop { folder: b, .. }) => a == b,
            _ => false,
        }
    }

    /// Whether the application is told about the drag here.
    const fn reported(&self) -> bool {
        matches!(self, Self::Window { .. } | Self::Desktop { .. })
    }
}

/// What one pointer event did to a carried drag.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DragStep {
    /// The pointer moved here; what is here is the caller's to resolve.
    Moved(Point),
    /// The drag ended.
    Ended(DragEnd),
    /// Nothing the drag acts on.
    Held,
}

/// A report a carried drag owes the application it began in: taken once per
/// batch of input ([`DesktopShell::take_drag_report`]), so a burst of motion
/// costs one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwedReport {
    /// The window the drag began in.
    pub source: u64,
    /// The report's number.
    pub serial: u32,
    /// Where the drag is.
    pub at: DragSurface,
    /// Whether `Shift` is held.
    pub shift: bool,
}

/// The one drag the seat carries, when there is one.
#[derive(Debug, Default)]
pub(crate) struct DragCarrier {
    carried: Option<Carried>,
}

#[derive(Debug)]
struct Carried {
    source: u64,
    items: DragItems,
    /// The compositor window of the plate naming what is carried.
    plate: Option<WindowId>,
    /// What the pointer is over.
    over: DragSurface,
    /// The last report's number.
    serial: u32,
    /// The number the first report about this place carries: an answer to an
    /// earlier one is about somewhere else.
    place_serial: u32,
    /// The report that last named a desktop folder.
    desktop_serial: Option<u32>,
    /// Whether a report is owed.
    owed: bool,
    /// What a drop here does, as the application last answered.
    shown: Option<DropOperation>,
    shift: bool,
}

impl Carried {
    /// The pointer shape the drag shows.
    const fn cursor(&self) -> CursorKind {
        match self.shown {
            Some(DropOperation::Copy) => CursorKind::DragCopy,
            Some(DropOperation::Move) => CursorKind::DragMove,
            None => CursorKind::Arrow,
        }
    }
}

impl DesktopShell {
    /// Whether a drag is being carried.
    #[must_use]
    pub const fn drag_active(&self) -> bool {
        self.drag.carried.is_some()
    }

    /// Begin carrying `items` out of the window-channel window `source`,
    /// whose compositor window `wm` holds the press that began it.
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
        items: DragItems,
    ) -> Result<(), Errno> {
        if self.drag_active() {
            return Err(Errno::AlreadyExists);
        }
        if self.router().pressed_in() != Some(wm) {
            return Err(Errno::PermissionDenied);
        }
        self.yield_pointer(compositor);
        let at = self.router().pointer();
        let plate = place_plate(
            None,
            &items,
            at,
            self.session().floating_theme(),
            compositor,
        );
        let shift = self.modifiers().shift;
        self.drag.carried = Some(Carried {
            source,
            items,
            plate,
            over: DragSurface::Nothing,
            serial: 0,
            place_serial: 1,
            desktop_serial: None,
            owed: false,
            shown: None,
            shift,
        });
        self.hold_drag_cursor(compositor);
        Ok(())
    }

    /// Carry the drag through one pointer `event`.
    ///
    /// A motion moves the plate and answers where to, for the caller to
    /// resolve what is there ([`drag_over`](Self::drag_over)); the press
    /// coming up drops the drag where it is, and any other press ends it with
    /// nothing dropped.
    pub fn drag_pointer(&mut self, compositor: &mut Compositor, event: &InputEvent) -> DragStep {
        let Some(carried) = self.drag.carried.as_ref() else {
            return DragStep::Held;
        };
        match *event {
            InputEvent::PointerMoved { to } => {
                let (plate, items) = (carried.plate, carried.items);
                self.track_pointer(to, compositor);
                let theme = self.session().floating_theme();
                let plate = place_plate(plate, &items, to, theme, compositor);
                if let Some(carried) = self.drag.carried.as_mut() {
                    carried.plate = plate;
                }
                DragStep::Moved(to)
            }
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            } => self
                .end_drag(compositor, true)
                .map_or(DragStep::Held, DragStep::Ended),
            InputEvent::PointerPressed { .. } => self
                .end_drag(compositor, false)
                .map_or(DragStep::Held, DragStep::Ended),
            _ => DragStep::Held,
        }
    }

    /// The window the carried drag began in and what it carries.
    #[must_use]
    pub fn drag_items(&self) -> Option<(u64, DragItems)> {
        self.drag
            .carried
            .as_ref()
            .map(|carried| (carried.source, carried.items))
    }

    /// Whether the carried drag's pointer is already over `place`, so nothing
    /// about it needs resolving again.
    #[must_use]
    pub fn drag_is_at(&self, place: DragPlace) -> bool {
        self.drag
            .carried
            .as_ref()
            .is_some_and(|carried| carried.over.place() == place)
    }

    /// Take what the pointer is over now. Arriving somewhere new lights or
    /// darkens a slot, and shows no verdict until the application answers
    /// for the new place; the report it is owed waits for
    /// [`take_drag_report`](Self::take_drag_report).
    pub fn drag_over(&mut self, compositor: &mut Compositor, next: DragSurface) {
        let Some(carried) = self.drag.carried.as_mut() else {
            return;
        };
        let arrived = !carried.over.same_place(&next);
        let changed = next != carried.over;
        let left_reported = arrived && carried.over.reported();
        carried.owed |= next.reported() && changed || left_reported;
        if arrived {
            carried.shown = None;
            carried.place_serial = carried.serial.saturating_add(1);
        }
        let lit = match &next {
            DragSurface::Slot {
                index,
                target: Some(_),
            } => Some(*index),
            _ => None,
        };
        carried.over = next;
        if arrived {
            let scale = compositor.scale();
            self.session_mut().taskbar_mut().set_drop_slot(lit, scale);
            self.hold_drag_cursor(compositor);
        }
    }

    /// Carry the drag through one key `event`: `Escape` ends it with nothing
    /// dropped, a change of `Shift` asks the application again, and every
    /// other key is the drag's to swallow.
    pub fn drag_key(&mut self, compositor: &mut Compositor, event: &InputEvent) -> Option<DragEnd> {
        match *event {
            InputEvent::KeyPressed {
                key: Key::Named(NamedKey::Escape),
                ..
            } => self.end_drag(compositor, false),
            InputEvent::KeyPressed { modifiers, .. }
            | InputEvent::KeyReleased { modifiers, .. }
            | InputEvent::ModifiersChanged { modifiers } => {
                let carried = self.drag.carried.as_mut()?;
                if carried.shift == modifiers.shift {
                    return None;
                }
                carried.shift = modifiers.shift;
                // An answer given for the other modifier says nothing about
                // what a drop does now, so the place is asked afresh.
                if carried.over.reported() {
                    carried.owed = true;
                    carried.shown = None;
                    carried.place_serial = carried.serial.saturating_add(1);
                    self.hold_drag_cursor(compositor);
                }
                None
            }
            _ => None,
        }
    }

    /// The report the carried drag owes its application, if one is owed:
    /// numbered, and marked sent.
    pub fn take_drag_report(&mut self) -> Option<OwedReport> {
        let carried = self.drag.carried.as_mut()?;
        if !carried.owed {
            return None;
        }
        carried.owed = false;
        carried.serial = carried.serial.saturating_add(1);
        if matches!(carried.over, DragSurface::Desktop { .. }) {
            carried.desktop_serial = Some(carried.serial);
        }
        Some(OwedReport {
            source: carried.source,
            serial: carried.serial,
            at: if carried.over.reported() {
                carried.over.clone()
            } else {
                DragSurface::Nothing
            },
            shift: carried.shift,
        })
    }

    /// Take the application's answer to the report numbered `serial` for the
    /// drag window `source` began: shown on the pointer while it still
    /// describes where the pointer is, and answering whether it was.
    pub fn drag_verdict(
        &mut self,
        compositor: &mut Compositor,
        source: u64,
        serial: u32,
        verdict: Option<DropOperation>,
    ) -> bool {
        let Some(carried) = self.drag.carried.as_mut() else {
            return false;
        };
        let current = carried.source == source
            && carried.over.reported()
            && (carried.place_serial..=carried.serial).contains(&serial);
        if !current {
            return false;
        }
        carried.shown = verdict;
        self.hold_drag_cursor(compositor);
        true
    }

    /// The pinboard icon a drop would land in, while the application accepts
    /// it there: the one the desktop lights.
    #[must_use]
    pub fn drag_drop_icon(&self) -> Option<usize> {
        let carried = self.drag.carried.as_ref()?;
        match carried.over {
            DragSurface::Desktop { icon, .. } if carried.shown.is_some() => icon,
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
        if matches!(carried.over, DragSurface::Slot { .. }) {
            carried.over = DragSurface::Nothing;
        }
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

    /// Stop carrying, answering how it ended: where the pointer is if
    /// `dropped` and what is there takes the drag — a slot's application, or a
    /// place whose last answer the pointer shows — and otherwise on nothing.
    /// The slot it lit is latched dark for the caller's settle to paint.
    pub fn end_drag(&mut self, compositor: &mut Compositor, dropped: bool) -> Option<DragEnd> {
        let carried = self.drag.carried.take()?;
        if let Some(plate) = carried.plate {
            compositor.remove(plate);
        }
        let scale = compositor.scale();
        self.session_mut().taskbar_mut().set_drop_slot(None, scale);
        self.cursor_mut().hold(None);
        self.refresh_cursor(compositor);
        let ended = match (dropped, &carried.over, carried.shown) {
            (
                true,
                DragSurface::Slot {
                    target: Some(target),
                    ..
                },
                _,
            ) => DragConclusion::Application(target.clone()),
            (true, &DragSurface::Window { window_id, x, y }, Some(operation)) => {
                DragConclusion::Window {
                    window_id,
                    x,
                    y,
                    operation,
                }
            }
            (true, DragSurface::Desktop { .. }, Some(operation)) => match carried.desktop_serial {
                Some(serial) => DragConclusion::Desktop { serial, operation },
                None => DragConclusion::Nothing,
            },
            _ => DragConclusion::Nothing,
        };
        Some(DragEnd {
            source: carried.source,
            ended,
        })
    }

    /// Show the shape the carried drag's verdict calls for, wherever the
    /// pointer goes.
    fn hold_drag_cursor(&mut self, compositor: &mut Compositor) {
        let Some(kind) = self.drag.carried.as_ref().map(Carried::cursor) else {
            return;
        };
        self.cursor_mut().hold(Some(kind));
        self.refresh_cursor(compositor);
    }
}

/// What the plate beside the pointer says: the one item's name, or how many
/// are carried.
fn plate_text(items: &DragItems) -> String {
    if items.count() == 1 {
        String::from(items.first().as_str())
    } else {
        format!("{} items", items.count())
    }
}

/// Show the plate naming `items` beside the pointer at `at`: `plate` moved
/// when it is still up, since what it says never changes, and painted anew
/// when it is not. Answers the plate, or `None` when the heap will not give
/// one — the drag is still carried, only unlabelled.
fn place_plate(
    plate: Option<WindowId>,
    items: &DragItems,
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
    let tip = Tooltip::new(plate_text(items));
    let (width, height) = tip.preferred_size(scale, theme);
    let mut pixels = Surface::new(width, height)?;
    tip.render(&mut pixels, Rect::new(0, 0, width, height), scale, theme);
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

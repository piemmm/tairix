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
//! The same carrier takes the document a trusted save panel names to a folder
//! (`plans/SAVE-DIALOG.md`): over the desktop the session resolves the folder
//! itself, and over a window it offers the save to that window's application,
//! which answers with a folder it opened itself. The panel then saves there
//! under its own authority.
//!
//! A drag is the press that began it, carried on: it holds the pointer until
//! that press comes up, and `Escape` or any other button ends it with nothing
//! dropped. The plate naming what is carried floats beside the pointer the way
//! the seat's tooltip plate does — input-transparent, above everything — so it
//! can never become the thing it is dropped on.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::window_ipc::{
    AlertSeverity, AlertSpec, DragItems, DropOperation, DropTarget, WindowEvent,
};
use tairix_abi::Errno;
use tairix_browse::media::{name_endings, MediaType};
use tairix_controls::{
    paint_icon_slot, paint_surface_plate, plate_border, ArtworkTone, ChromeLayer, Tooltip,
};
use tairix_font::{BitmapFont, ELLIPSIS};
use tairix_geometry::{Point, Rect, Scale};
use tairix_icon::IconKind;
use tairix_theme::{CursorKind, TextRole, Theme};
use tairix_util::utf8::shown;
use tairix_window::DragConclusion;
use tairix_wm::{
    Compositor, Corners, InputEvent, Key, NamedKey, PointerButton, PointerCatch, Surface, WindowId,
};

use crate::shell::DesktopShell;

/// How far the plate sits from the pointer's hotspot, right and down, in
/// *logical* pixels: clear of the cursor it follows.
const PLATE_OFFSET: u32 = 14;

/// The side of the document picture a save's plate carries, in logical
/// pixels.
const SAVE_PLATE_ICON: u32 = 32;

/// The widest a save's plate shows the document's name, in logical pixels.
const SAVE_PLATE_NAME_MAX: u32 = 240;

/// How a carried drag ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DragEnd {
    /// An application's drag of its own items.
    Items(ItemsEnd),
    /// A save carried from the trusted picker's panel.
    Save(SaveEnd),
}

/// How an application's drag ended: the window it began in, where it was
/// dropped, and why the slot it was dropped on refused it, when one did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ItemsEnd {
    /// The window-channel id of the window the drag began in.
    pub source: u64,
    /// Where it was dropped.
    pub ended: DragConclusion,
    /// Why the application whose slot it was dropped on does not open it,
    /// for the desktop to say in that application's name.
    pub refused: Option<Box<DropRefusal>>,
}

/// How a carried save ended: where it was dropped, and the end the window it
/// was last offered to is owed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SaveEnd {
    /// Where the document goes.
    pub dropped: SaveDrop,
    /// The window last offered the save, to be told the offer ended.
    pub offer: Option<OfferEnd>,
}

/// Where a carried save was dropped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SaveDrop {
    /// Nowhere that saves it.
    Nothing,
    /// On a window that had not yet said whether it takes the save.
    Unanswered,
    /// Into the desktop folder at this absolute path.
    Folder(String),
    /// On the window its offer numbered `serial` stood over, whose
    /// application names the folder.
    Offer {
        /// The window dropped on.
        window_id: u64,
        /// The offer that stood there.
        serial: u32,
    },
}

/// The document a save drag carries: what its plate shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SavedDocument {
    /// The name it is saved under.
    pub name: String,
    /// Its format's picture.
    pub icon: IconKind,
}

/// A save offered to the window under the pointer, at a point in it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SaveOffer {
    /// The window offered it.
    pub window_id: u64,
    /// The offer's number.
    pub serial: u32,
    /// Window-local x.
    pub x: u32,
    /// Window-local y.
    pub y: u32,
}

/// The end a window offered a save is owed: it left, or was dropped on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct OfferEnd {
    /// The window offered it.
    pub window_id: u64,
    /// The last offer made over it.
    pub serial: u32,
    /// Where on it the save was dropped, window-local, or `None` when it
    /// left.
    pub dropped: Option<(u32, u32)>,
}

impl SaveOffer {
    /// The event that makes the offer.
    #[must_use]
    pub const fn event(self) -> WindowEvent {
        WindowEvent::SaveOffered {
            window_id: self.window_id,
            serial: self.serial,
            x: self.x,
            y: self.y,
        }
    }
}

impl OfferEnd {
    /// The event that tells the window.
    #[must_use]
    pub const fn event(self) -> WindowEvent {
        WindowEvent::SaveOfferEnded {
            window_id: self.window_id,
            serial: self.serial,
            dropped: self.dropped,
        }
    }
}

/// What a slot's application answers a drag with.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SlotAnswer {
    /// It opens the one file dragged. Boxed: a target carries a whole bundle
    /// path, wider than every other surface.
    Opens(Box<DropTarget>),
    /// It does not, for the reason a drop there is answered with.
    Refuses(Box<DropRefusal>),
}

/// Why an application does not take what was dropped on its slot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DropRefusal {
    app: String,
    item: String,
    one_file: bool,
    opens: Vec<&'static str>,
    more: bool,
}

/// The most file endings a refusal names before it says there are others.
const OPENS_NAMED: usize = 8;

impl DropRefusal {
    /// `app`, which claims the media types `claims`, refusing what is led by
    /// `item`: one file of a type it does not claim when `one_file`, else
    /// several items or a folder.
    #[must_use]
    pub fn new(app: &str, item: &str, one_file: bool, claims: &[String]) -> Self {
        let mut opens: Vec<&'static str> = Vec::new();
        let mut more = false;
        let endings = claims
            .iter()
            .filter_map(|claim| MediaType::from_media_str(claim))
            .flat_map(name_endings)
            .filter(|(mark, _)| *mark == '.')
            .map(|(_, ending)| ending);
        for ending in endings {
            if opens.contains(&ending) {
                continue;
            }
            if opens.len() == OPENS_NAMED {
                more = true;
                break;
            }
            opens.push(ending);
        }
        Self {
            app: String::from(app),
            item: String::from(item),
            one_file,
            opens,
            more,
        }
    }

    /// The application refusing, by its attested name.
    #[must_use]
    pub fn app(&self) -> &str {
        &self.app
    }

    /// The alert the desktop states the refusal in, or `None` for a label no
    /// alert can carry — a compiled-in word, so a defect the tests catch.
    ///
    /// The item and the application are named as shown, since a developer
    /// chooses an application's name and anyone a file's.
    #[must_use]
    pub fn spec(&self) -> Option<AlertSpec> {
        let (app, item) = (shown(&self.app), shown(&self.item));
        let headline = format!("{app} cannot open \u{201c}{item}\u{201d}");
        let detail = if !self.one_file {
            format!("{app} opens one file at a time, never a folder or several items at once.")
        } else if self.opens.is_empty() {
            format!("{app} opens no files dropped on it.")
        } else {
            format!("{app} opens files ending {}.", self.endings())
        };
        AlertSpec::acknowledged(AlertSeverity::Error, &headline, &detail).ok()
    }

    /// The endings the application opens, as a phrase.
    fn endings(&self) -> String {
        let named: Vec<String> = self
            .opens
            .iter()
            .map(|ending| format!(".{ending}"))
            .collect();
        match (named.split_last(), self.more) {
            (Some((last, [])), false) => last.clone(),
            (Some((last, rest)), false) => format!("{} and {last}", rest.join(", ")),
            (Some(_), true) => format!("{} and others", named.join(", ")),
            (None, _) => String::new(),
        }
    }
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
        /// What the slot's application answers the drag with, or `None` for
        /// a slot the desktop cannot vouch for.
        answer: Option<SlotAnswer>,
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
    /// A window of another application: what it answers is what its slot at
    /// `index` answers, since a window takes a drop as its application does.
    AppWindow {
        /// The application's slot in the strip.
        index: usize,
        /// What the application answers the drag with, or `None` when the
        /// desktop cannot vouch for it.
        answer: Option<SlotAnswer>,
    },
    /// A window a carried save is offered to, at a point in it.
    Offer {
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
    /// A window of the application at slot `index`, which is not the one
    /// dragging.
    AppWindow {
        /// The application's slot in the strip.
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
    /// A window a carried save is offered to, at a point in it.
    Offer {
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

/// What a carried drag finds over a window: who owns it, and whether an
/// alert holds it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WindowUnder {
    /// The dragging application's own window, at a window-local point in its
    /// client, or `None` over its furniture.
    Own(Option<(u32, u32)>),
    /// Another application's window, that application at slot `index` when
    /// the strip shows it.
    Foreign(Option<usize>),
}

impl DragPlace {
    /// Where a drag over window-channel window `window_id` lands: nothing
    /// while an alert holds the window — it takes nothing until answered —
    /// else the window itself for the dragging application's own, else the
    /// other application's slot.
    #[must_use]
    pub const fn over_window(window_id: u64, held: bool, under: WindowUnder) -> Self {
        match (held, under) {
            (true, _) | (false, WindowUnder::Own(None) | WindowUnder::Foreign(None)) => {
                Self::Nothing
            }
            (false, WindowUnder::Own(Some((x, y)))) => Self::Window { window_id, x, y },
            (false, WindowUnder::Foreign(Some(index))) => Self::AppWindow { index },
        }
    }
}

impl DragSurface {
    /// The place this is what is over.
    #[must_use]
    pub const fn place(&self) -> DragPlace {
        match *self {
            Self::Nothing => DragPlace::Nothing,
            Self::Slot { index, .. } => DragPlace::Slot { index },
            Self::AppWindow { index, .. } => DragPlace::AppWindow { index },
            Self::Window { window_id, x, y } => DragPlace::Window { window_id, x, y },
            Self::Offer { window_id, x, y } => DragPlace::Offer { window_id, x, y },
            Self::Desktop { icon, revision, .. } => DragPlace::Desktop { icon, revision },
        }
    }

    /// Whether `self` and `other` are the same place to drop, wherever on it
    /// the pointer is: a verdict for one still holds for the other.
    fn same_place(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Nothing, Self::Nothing) => true,
            (Self::Slot { index: a, .. }, Self::Slot { index: b, .. })
            | (Self::AppWindow { index: a, .. }, Self::AppWindow { index: b, .. }) => a == b,
            (Self::Window { window_id: a, .. }, Self::Window { window_id: b, .. })
            | (Self::Offer { window_id: a, .. }, Self::Offer { window_id: b, .. }) => a == b,
            (Self::Desktop { folder: a, .. }, Self::Desktop { folder: b, .. }) => a == b,
            _ => false,
        }
    }

    /// Whether an application is told about the drag here.
    const fn reported(&self) -> bool {
        matches!(
            self,
            Self::Window { .. } | Self::Offer { .. } | Self::Desktop { .. }
        )
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

/// What a carried drag owes the applications it concerns: taken once per
/// batch of input ([`DesktopShell::take_drag_report`]), so a burst of motion
/// costs one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OwedReport {
    /// Where an application's drag is, for the application it began in.
    Items(ItemsReport),
    /// The offers a carried save owes.
    Save(SaveReport),
}

/// What a carried save owes: an end to the window it left, and an offer to the
/// one it is over. Owed with neither when it only came to or left the
/// desktop, whose folder the desktop lights itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SaveReport {
    /// The window it left.
    pub left: Option<OfferEnd>,
    /// The window it is over, and where.
    pub over: Option<SaveOffer>,
}

/// A report an application's drag owes the window it began in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ItemsReport {
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
    /// The number the seat last gave a report. A drag numbers its reports on
    /// from it, so a late answer to an earlier drag never names one of this
    /// drag's.
    issued: u32,
}

/// What a carried drag holds.
#[derive(Debug)]
enum Cargo {
    /// Items the application's window `source` began dragging, boxed so the
    /// carrier is no larger than a save.
    Items { source: u64, items: Box<DragItems> },
    /// The document the trusted picker's save panel names.
    Save {
        /// The window-channel window the save is for.
        requester: u64,
        document: SavedDocument,
        /// The window offered the save last and that offer's number, until
        /// it is told the offer ended.
        offered: Option<(u64, u32)>,
    },
}

#[derive(Debug)]
struct Carried {
    cargo: Cargo,
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
    /// Whether the place has answered since the pointer reached it.
    answered: bool,
    shift: bool,
}

impl Carried {
    /// Whether `serial` numbers a report made since the pointer reached the
    /// place it is over, counting across the numbers' wrap: none has, until
    /// the first report there.
    const fn reported_here(&self, serial: u32) -> bool {
        let made = self.serial.wrapping_sub(self.place_serial).wrapping_add(1);
        serial.wrapping_sub(self.place_serial) < made
    }

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
        let items = Box::new(items);
        self.carry(compositor, wm, Cargo::Items { source, items })
    }

    /// Begin carrying the save the picker's panel, in its compositor window
    /// `panel`, names for the window-channel window `requester`.
    ///
    /// # Errors
    ///
    /// As [`begin_drag`](Self::begin_drag), the press held in `panel`.
    pub fn begin_save_drag(
        &mut self,
        compositor: &mut Compositor,
        (panel, requester): (WindowId, u64),
        document: SavedDocument,
    ) -> Result<(), Errno> {
        let cargo = Cargo::Save {
            requester,
            document,
            offered: None,
        };
        self.carry(compositor, panel, cargo)
    }

    /// Carry `cargo` on from the press held in `wm`.
    fn carry(
        &mut self,
        compositor: &mut Compositor,
        wm: WindowId,
        cargo: Cargo,
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
            &cargo,
            at,
            self.session().floating_theme(),
            compositor,
        );
        let shift = self.modifiers().shift;
        let issued = self.drag.issued;
        self.drag.carried = Some(Carried {
            cargo,
            plate,
            over: DragSurface::Nothing,
            serial: issued,
            place_serial: issued.wrapping_add(1),
            desktop_serial: None,
            owed: false,
            shown: None,
            answered: false,
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
        if self.drag.carried.is_none() {
            return DragStep::Held;
        }
        match *event {
            InputEvent::PointerMoved { to } => {
                self.track_pointer(to, compositor);
                self.place_drag_plate(compositor, to);
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

    /// The window the carried drag began in and what it carries, for an
    /// application's drag.
    #[must_use]
    pub fn drag_items(&self) -> Option<(u64, DragItems)> {
        match &self.drag.carried.as_ref()?.cargo {
            Cargo::Items { source, items } => Some((*source, **items)),
            Cargo::Save { .. } => None,
        }
    }

    /// The window-channel window a carried save is for, which it is never
    /// dropped on.
    #[must_use]
    pub fn save_source(&self) -> Option<u64> {
        match self.drag.carried.as_ref()?.cargo {
            Cargo::Save { requester, .. } => Some(requester),
            Cargo::Items { .. } => None,
        }
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
        let saving = matches!(carried.cargo, Cargo::Save { .. });
        // The desktop takes a save itself, so arriving there accepts it at
        // once and owes no application anything; the report owed only relights
        // its folder.
        carried.owed |= next.reported() && changed || left_reported;
        if arrived {
            carried.shown = match next {
                DragSurface::Desktop { .. } if saving => Some(DropOperation::Copy),
                _ => None,
            };
            carried.answered = false;
            carried.place_serial = carried.serial.wrapping_add(1);
        }
        let lit = match &next {
            DragSurface::Slot {
                index,
                answer: Some(SlotAnswer::Opens(_)),
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
                // A save is a save whatever is held.
                if carried.shift == modifiers.shift || matches!(carried.cargo, Cargo::Save { .. }) {
                    return None;
                }
                carried.shift = modifiers.shift;
                // An answer given for the other modifier says nothing about
                // what a drop does now, so the place is asked afresh.
                if carried.over.reported() {
                    carried.owed = true;
                    carried.shown = None;
                    carried.answered = false;
                    carried.place_serial = carried.serial.wrapping_add(1);
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
        let source = match &mut carried.cargo {
            Cargo::Items { source, .. } => *source,
            Cargo::Save { offered, .. } => {
                let over = match carried.over {
                    DragSurface::Offer { window_id, x, y } => Some((window_id, x, y)),
                    _ => None,
                };
                let left = offered
                    .take_if(|(window, _)| over.is_none_or(|(now, ..)| now != *window))
                    .map(|(window_id, serial)| OfferEnd {
                        window_id,
                        serial,
                        dropped: None,
                    });
                let over = over.map(|(window_id, x, y)| {
                    carried.serial = carried.serial.wrapping_add(1);
                    *offered = Some((window_id, carried.serial));
                    SaveOffer {
                        window_id,
                        serial: carried.serial,
                        x,
                        y,
                    }
                });
                return Some(OwedReport::Save(SaveReport { left, over }));
            }
        };
        carried.serial = carried.serial.wrapping_add(1);
        if matches!(carried.over, DragSurface::Desktop { .. }) {
            carried.desktop_serial = Some(carried.serial);
        }
        Some(OwedReport::Items(ItemsReport {
            source,
            serial: carried.serial,
            at: if carried.over.reported() {
                carried.over.clone()
            } else {
                DragSurface::Nothing
            },
            shift: carried.shift,
        }))
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
        let current = matches!(carried.cargo, Cargo::Items { source: from, .. } if from == source)
            && carried.over.reported()
            && carried.reported_here(serial);
        if !current {
            return false;
        }
        carried.shown = verdict;
        carried.answered = true;
        self.hold_drag_cursor(compositor);
        true
    }

    /// Take the answer window `window_id`'s application gave the save offer
    /// numbered `serial`: shown on the pointer while the save is still over
    /// that window and the offer is the one standing there, and answering
    /// whether it was.
    pub fn save_offer_answered(
        &mut self,
        compositor: &mut Compositor,
        window_id: u64,
        serial: u32,
        accepts: bool,
    ) -> bool {
        let Some(carried) = self.drag.carried.as_mut() else {
            return false;
        };
        let current = matches!(carried.cargo, Cargo::Save { .. })
            && matches!(carried.over, DragSurface::Offer { window_id: over, .. } if over == window_id)
            && carried.reported_here(serial);
        if !current {
            return false;
        }
        carried.shown = accepts.then_some(DropOperation::Copy);
        carried.answered = true;
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
        if matches!(
            carried.over,
            DragSurface::Slot { .. } | DragSurface::AppWindow { .. }
        ) {
            carried.over = DragSurface::Nothing;
        }
        self.session_mut().taskbar_mut().set_drop_slot(None, scale);
    }

    /// Follow the window-channel window `window` going: a drag it began ends
    /// with nothing to tell it, as does a save for it, and a save offered to it
    /// forgets it. Answers the end a save leaves another window owed.
    pub fn abort_drag_for(&mut self, compositor: &mut Compositor, window: u64) -> Option<OfferEnd> {
        let carried = self.drag.carried.as_mut()?;
        let ends = match &mut carried.cargo {
            Cargo::Items { source, .. } => *source == window,
            Cargo::Save {
                requester, offered, ..
            } => {
                if offered.is_some_and(|(offered, _)| offered == window) {
                    *offered = None;
                }
                *requester == window
            }
        };
        if ends {
            return match self.end_drag(compositor, false)? {
                DragEnd::Save(end) => end.offer,
                DragEnd::Items(_) => None,
            };
        }
        if matches!(carried.over, DragSurface::Offer { window_id, .. } if window_id == window) {
            carried.over = DragSurface::Nothing;
            carried.shown = None;
            carried.answered = false;
            self.hold_drag_cursor(compositor);
        }
        None
    }

    /// Paint the carried drag's plate afresh where the pointer is: the name it
    /// shows was drawn provisionally and its text has landed.
    pub fn redraw_drag_plate(&mut self, compositor: &mut Compositor) {
        let Some(old) = self
            .drag
            .carried
            .as_mut()
            .map(|carried| carried.plate.take())
        else {
            return;
        };
        if let Some(old) = old {
            compositor.remove(old);
        }
        let at = self.router().pointer();
        self.place_drag_plate(compositor, at);
    }

    /// Show the carried drag's plate beside the pointer at `at`.
    fn place_drag_plate(&mut self, compositor: &mut Compositor, at: Point) {
        let Some(carried) = self.drag.carried.as_ref() else {
            return;
        };
        let theme = self.session().floating_theme();
        let plate = place_plate(carried.plate, &carried.cargo, at, theme, compositor);
        if let Some(carried) = self.drag.carried.as_mut() {
            carried.plate = plate;
        }
    }

    /// Stop carrying, answering how it ended: where the pointer is if
    /// `dropped` and what is there takes the drag — a slot's application, or a
    /// place whose last answer the pointer shows — and otherwise on nothing.
    /// The slot it lit is latched dark for the caller's settle to paint.
    pub fn end_drag(&mut self, compositor: &mut Compositor, dropped: bool) -> Option<DragEnd> {
        let carried = self.drag.carried.take()?;
        self.drag.issued = carried.serial;
        if let Some(plate) = carried.plate {
            compositor.remove(plate);
        }
        let scale = compositor.scale();
        self.session_mut().taskbar_mut().set_drop_slot(None, scale);
        self.cursor_mut().hold(None);
        self.refresh_cursor(compositor);
        let source = match carried.cargo {
            Cargo::Items { source, .. } => source,
            Cargo::Save { offered, .. } => {
                return Some(DragEnd::Save(save_end(
                    dropped,
                    &carried.over,
                    (carried.shown, carried.answered),
                    offered,
                )));
            }
        };
        let answer = match &carried.over {
            DragSurface::Slot { answer, .. } | DragSurface::AppWindow { answer, .. } => {
                answer.as_ref()
            }
            _ => None,
        };
        let refused = match (dropped, answer) {
            (true, Some(SlotAnswer::Refuses(refusal))) => Some(refusal.clone()),
            _ => None,
        };
        let ended = match (dropped, answer, &carried.over, carried.shown) {
            (true, Some(SlotAnswer::Opens(target)), _, _) => {
                DragConclusion::Application(target.clone())
            }
            (true, _, &DragSurface::Window { window_id, x, y }, Some(operation)) => {
                DragConclusion::Window {
                    window_id,
                    x,
                    y,
                    operation,
                }
            }
            (true, _, DragSurface::Desktop { .. }, Some(operation)) => {
                match carried.desktop_serial {
                    Some(serial) => DragConclusion::Desktop { serial, operation },
                    None => DragConclusion::Nothing,
                }
            }
            _ => DragConclusion::Nothing,
        };
        Some(DragEnd::Items(ItemsEnd {
            source,
            ended,
            refused,
        }))
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

/// How a carried save ended over `over`, showing `shown` and whether the
/// place had answered, the offer `offered` standing: dropped on the desktop, it
/// goes in the folder there; on a window whose application accepted the offer
/// standing, in the folder it names; on one yet to answer, nowhere, for the
/// panel to say so; and anywhere else, nowhere.
fn save_end(
    dropped: bool,
    over: &DragSurface,
    (shown, answered): (Option<DropOperation>, bool),
    offered: Option<(u64, u32)>,
) -> SaveEnd {
    let (dropped, at) = match (dropped, over, shown, offered) {
        (true, DragSurface::Desktop { folder, .. }, Some(_), _) => {
            (SaveDrop::Folder(folder.clone()), None)
        }
        (true, &DragSurface::Offer { window_id, x, y }, Some(_), Some((offer, serial)))
            if offer == window_id =>
        {
            (SaveDrop::Offer { window_id, serial }, Some((x, y)))
        }
        (true, DragSurface::Offer { .. }, None, _) if !answered => (SaveDrop::Unanswered, None),
        _ => (SaveDrop::Nothing, None),
    };
    let offer = offered.map(|(window_id, serial)| OfferEnd {
        window_id,
        serial,
        dropped: at,
    });
    SaveEnd { dropped, offer }
}

/// What the plate beside the pointer says: the one item's name, or how many
/// are carried.
fn plate_text(items: &DragItems) -> String {
    if items.count() == 1 {
        shown(items.first().as_str()).into_owned()
    } else {
        format!("{} items", items.count())
    }
}

/// Show the plate naming what `cargo` carries beside the pointer at `at`:
/// `plate` moved when it is still up, since what it says never changes, and
/// painted anew when it is not. Answers the plate, or `None` when the heap will
/// not give one — the drag is still carried, only unlabelled.
fn place_plate(
    plate: Option<WindowId>,
    cargo: &Cargo,
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
    let (pixels, radius) = match cargo {
        Cargo::Items { items, .. } => {
            let tip = Tooltip::new(plate_text(items));
            let (width, height) = tip.preferred_size(scale, theme);
            let mut pixels = Surface::new(width, height)?;
            tip.render(&mut pixels, Rect::new(0, 0, width, height), scale, theme);
            (
                pixels,
                scale.scale_length(theme.metrics().popup_corner_radius),
            )
        }
        Cargo::Save { document, .. } => save_plate(document, scale, theme)?,
    };
    let id = compositor.add_window(origin, pixels);
    compositor.set_pointer_catch(id, PointerCatch::None);
    compositor.set_corners(id, Corners::painted(radius));
    compositor.set_casts_shadow(id, true);
    compositor.raise(id);
    Some(id)
}

/// The plate a save carries, and its corner radius: the document's picture
/// and the name it is saved under, on the raised plate a dragged item's name
/// is drawn on. `None` when the heap will not give one.
fn save_plate(document: &SavedDocument, scale: Scale, theme: &Theme) -> Option<(Surface, u32)> {
    let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, scale);
    let metrics = theme.metrics();
    let border = plate_border(theme, scale);
    let margin = border.saturating_add(scale.scale_length(metrics.control_inset).max(1));
    let gap = scale.scale_length(metrics.control_gap);
    let side = scale.scale_length(SAVE_PLATE_ICON);
    let name = shown(&document.name);
    let (fitted, elided) = font.elide_to_width(&name, scale.scale_length(SAVE_PLATE_NAME_MAX));
    let mark = if elided { font.text_width(ELLIPSIS) } else { 0 };
    let text_x = margin.saturating_add(side).saturating_add(gap);
    let width = text_x
        .saturating_add(font.text_width(fitted))
        .saturating_add(mark)
        .saturating_add(margin);
    let height = margin
        .saturating_mul(2)
        .saturating_add(side.max(font.line_height()));
    let radius = scale
        .scale_length(metrics.popup_corner_radius)
        .min(height / 2);
    let mut pixels = Surface::new(width, height)?;
    let palette = theme.palette();
    let _ = paint_surface_plate(
        &mut pixels,
        (0, 0, width, height),
        (radius, border),
        theme,
        (palette.surface_raised, ChromeLayer::Ground),
    );
    paint_icon_slot(
        &mut pixels,
        (margin, height.saturating_sub(side) / 2, side),
        document.icon,
        palette.accent.into(),
        None,
        ArtworkTone::FULL,
    );
    let ink = palette.on_surface.into();
    let x = i32::try_from(text_x).unwrap_or(i32::MAX);
    let y = i32::try_from(height.saturating_sub(font.line_height()) / 2).unwrap_or(0);
    let pen = font.draw_text(&mut pixels, x, y, fitted, ink);
    if elided {
        font.draw_text(&mut pixels, pen, y, ELLIPSIS, ink);
    }
    Some((pixels, radius))
}

#[cfg(test)]
#[path = "drag_tests.rs"]
mod tests;

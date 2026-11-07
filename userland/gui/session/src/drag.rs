//! The seat's drag carrier: items the user drags out of an application,
//! carried by the desktop to where they are dropped.
//!
//! What reaches the session of the items is what it shows of them and what an
//! application slot's declared types are matched against: the pressed item's
//! name, the ending its listing hides of it and the filetype its volume
//! records, the count, and the class picture of each item shown with where it
//! stood, a bundle's or a RISC OS application's own picture with it by its path
//! (`DragItems`). Over one of the dragging application's own windows, or over
//! the desktop, the session reports where the drag is and shows on the pointer
//! the verdict the application answers: copy, move, or nothing. The
//! application keeps the items and performs every drop itself, so no authority
//! crosses to the desktop: an own picture is read under the session's own
//! identity and nothing of it returns, and the only other path that crosses is
//! the desktop's own folder, which the session names and the application
//! already reaches.
//!
//! The same carrier takes the document a trusted save panel names to a folder
//! (`plans/SAVE-DIALOG.md`): over the desktop the session resolves the folder
//! itself, and over a window it offers the save to that window's application,
//! which answers with a folder it opened itself. The panel then saves there
//! under its own authority.
//!
//! It also takes icons carried off the desktop over a window: they are offered
//! to that window's application, which answers what a drop there would do,
//! reads the items' paths, and performs the drop itself under its own
//! authority, as a drop of its own items onto the desktop is performed.
//! Brought back over the desktop and let go, they are put down there.
//!
//! A drag is the press that began it, carried on: it holds the pointer until
//! that press comes up, and `Escape` or any other button ends it with nothing
//! dropped. What is carried is shown under the pointer as its listing shows it
//! (`stack`) — one item as its tile, held where it was pressed, several as a
//! stack gathering from where they stood — in windows nothing can be dropped
//! on, above everything, so it can never become the thing it is dropped on.

mod stack;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::driver::display::RefreshRate;
use tairix_abi::fs::FileId;
use tairix_abi::window_ipc::{
    AlertSeverity, AlertSpec, DragItems, DropOperation, DropTarget, OfferDrop, OfferedItem,
    WindowEvent,
};
use tairix_abi::Errno;
use tairix_browse::media::{name_endings, MediaType};
use tairix_browse::render::Tiles;
use tairix_controls::{ControlState, SelectionState};
use tairix_geometry::{Point, Rect};
use tairix_icon::{IconKind, IconRequest, Landed};
use tairix_theme::CursorKind;
use tairix_util::utf8::shown;
use tairix_window::DragConclusion;
use tairix_wm::{Compositor, InputEvent, Key, NamedKey, PointerButton, WindowId};

use crate::picker::PickId;
use crate::shell::DesktopShell;
use crate::switchuser::park_within;

pub use stack::PictureGrab;
use stack::{Look, Stack};

/// How a carried drag ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DragEnd {
    /// An application's drag of its own items.
    Items(ItemsEnd),
    /// A save carried from the trusted picker's panel.
    Save(SaveEnd),
    /// Icons carried off the desktop.
    Desktop(DesktopEnd),
}

/// How a drag of icons carried off the desktop ended: where they were
/// dropped, and the end the window last offered them is owed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DesktopEnd {
    /// Where they were dropped.
    pub dropped: DesktopDrop,
    /// The window last offered them, to be told the offer ended.
    pub offer: Option<ItemsOfferEnd>,
}

/// Where icons carried off the desktop were dropped.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DesktopDrop {
    /// Nowhere that takes them: they stand where they stood.
    Nothing,
    /// Back on the desktop, at this point, where they are put down.
    Desktop(Point),
    /// On a window whose application answered that it takes them, which
    /// performs the drop.
    Window,
}

/// What a drag of icons carried off the desktop carries: what it shows of
/// them, and each item as the window offered them reads it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopCargo {
    /// What it shows.
    pub shown: DragItems,
    /// Each item, the pressed one first.
    pub items: Vec<DesktopItem>,
}

/// One icon carried off the desktop: the absolute path a drop acts on — a
/// drive's is its volume's place in the storage catalog — and the node the
/// desktop's listing showed there, or [`FileId::NONE`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopItem {
    /// Its absolute path.
    pub path: String,
    /// The node listed there.
    pub id: FileId,
}

/// Items carried off the desktop, offered to the window under the pointer at
/// a point in it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ItemsOffer {
    /// The window offered them.
    pub window_id: u64,
    /// The drag's number, which the window reads the items by.
    pub drag: u32,
    /// The offer's number.
    pub serial: u32,
    /// Window-local x.
    pub x: u32,
    /// Window-local y.
    pub y: u32,
    /// Whether `Shift` is held.
    pub shift: bool,
}

impl ItemsOffer {
    /// The event that makes the offer.
    #[must_use]
    pub const fn event(self) -> WindowEvent {
        WindowEvent::ItemsOffered {
            window_id: self.window_id,
            drag: self.drag,
            serial: self.serial,
            x: self.x,
            y: self.y,
            shift: self.shift,
        }
    }
}

/// The end a window offered items off the desktop is owed: they left, or
/// were dropped on it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ItemsOfferEnd {
    /// The window offered them.
    pub window_id: u64,
    /// The last offer made over it.
    pub serial: u32,
    /// Where on it they were dropped, window-local, and what the drop does,
    /// or `None` when they left.
    pub dropped: Option<OfferDrop>,
}

impl ItemsOfferEnd {
    /// The event that tells the window.
    #[must_use]
    pub const fn event(self) -> WindowEvent {
        WindowEvent::ItemsOfferEnded {
            window_id: self.window_id,
            serial: self.serial,
            dropped: self.dropped,
        }
    }
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

/// How a carried save ended: the pick it was carried for, where it was
/// dropped, and the end the window it was last offered to is owed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SaveEnd {
    /// The pick whose panel it was carried out of.
    pub pick: PickId,
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

/// The document a save drag carries: what it is shown as.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SavedDocument {
    /// The name it is saved under.
    pub name: String,
    /// Its format's picture.
    pub icon: IconKind,
    /// Where on its picture the press that carried it out held it.
    pub grab: PictureGrab,
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

    /// Whether a report of `self` tells its application what one of `other`
    /// told it: the place and the point on it, whatever of the desktop's own
    /// listing the desktop folder there was resolved from.
    fn reports_as(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Desktop { folder: a, .. }, Self::Desktop { folder: b, .. }) => a == b,
            _ => self == other,
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
    /// The offers icons carried off the desktop owe.
    Desktop(DesktopReport),
}

/// What icons carried off the desktop owe: an end to the window they left,
/// and an offer to the one they are over.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopReport {
    /// The window they left.
    pub left: Option<ItemsOfferEnd>,
    /// The window they are over, and where.
    pub over: Option<ItemsOffer>,
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
    /// The number the last drag of icons off the desktop was given.
    desktop_drags: u32,
    /// The items the last drag off the desktop dropped on a window, which
    /// that window's application reads until the next such drag.
    dropped: Option<Dropped>,
}

/// Items dropped off the desktop on a window, for its application to read.
#[derive(Debug)]
struct Dropped {
    window_id: u64,
    drag: u32,
    items: Vec<DesktopItem>,
}

/// What a carried drag holds.
#[derive(Debug)]
enum Cargo {
    /// Items the application's window `source` began dragging, boxed so the
    /// carrier is no larger than a save.
    Items { source: u64, items: Box<DragItems> },
    /// The document a pick's save panel names.
    Save {
        /// The pick whose panel it was carried out of.
        pick: PickId,
        /// The window-channel window the save is for.
        requester: u64,
        document: SavedDocument,
        /// The window offered the save last and that offer's number, until
        /// it is told the offer ended.
        offered: Option<(u64, u32)>,
    },
    /// Icons carried off the desktop, boxed so the carrier is no larger than
    /// a save.
    Desktop {
        /// The drag's number.
        drag: u32,
        cargo: Box<DesktopCargo>,
        /// The window offered them last and that offer's number, until it is
        /// told the offer ended.
        offered: Option<(u64, u32)>,
    },
}

#[derive(Debug)]
struct Carried {
    cargo: Cargo,
    /// What is shown of what is carried, under the pointer.
    stack: Stack,
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
            Some(DropOperation::Link) => CursorKind::DragLink,
            None => CursorKind::Arrow,
        }
    }
}

impl DragCarrier {
    /// Hand `ask` the picture each card of the carried drag is drawn by and how
    /// many pixels across; nothing while no drag is carried.
    pub(crate) fn each_picture(
        &self,
        look: (tairix_geometry::Scale, &tairix_theme::Theme),
        ask: impl FnMut(IconRequest<'_>, u32),
    ) {
        if let Some(carried) = &self.carried {
            carried.stack.each_picture(look, ask);
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

    /// Begin carrying the save the panel of pick `pick`, in its compositor
    /// window `panel`, names for the window-channel window `requester`.
    ///
    /// # Errors
    ///
    /// As [`begin_drag`](Self::begin_drag), the press held in `panel`.
    pub fn begin_save_drag(
        &mut self,
        compositor: &mut Compositor,
        (panel, pick, requester): (WindowId, PickId, u64),
        document: SavedDocument,
    ) -> Result<(), Errno> {
        let cargo = Cargo::Save {
            pick,
            requester,
            document,
            offered: None,
        };
        self.carry(compositor, panel, cargo)
    }

    /// Carry the icons `cargo` describes on from the press on the desktop that
    /// carries them, now over a window: offered to whatever window they are
    /// held over, and put down again on the desktop if let go there.
    ///
    /// # Errors
    ///
    /// * [`Errno::AlreadyExists`] — a drag is already being carried.
    /// * [`Errno::PermissionDenied`] — the primary button is not held: a drag
    ///   is a press carried on.
    pub fn begin_desktop_drag(
        &mut self,
        compositor: &mut Compositor,
        cargo: DesktopCargo,
    ) -> Result<(), Errno> {
        if self.drag_active() {
            return Err(Errno::AlreadyExists);
        }
        if !self.router().holds_primary() {
            return Err(Errno::PermissionDenied);
        }
        self.drag.desktop_drags = self.drag.desktop_drags.wrapping_add(1);
        self.drag.dropped = None;
        let cargo = Cargo::Desktop {
            drag: self.drag.desktop_drags,
            cargo: Box::new(cargo),
            offered: None,
        };
        let field = self.work_area(compositor);
        self.lift(compositor, cargo, Some(field));
        Ok(())
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
        let source = compositor.window(wm).map(tairix_wm::Window::client_rect);
        self.lift(compositor, cargo, source);
        Ok(())
    }

    /// Take the pointer and show `cargo` under it, carried out of `source`.
    fn lift(&mut self, compositor: &mut Compositor, cargo: Cargo, source: Option<Rect>) {
        self.yield_pointer(compositor);
        let at = self.router().pointer();
        let scale = compositor.scale();
        let (theme, mut artwork) = self.theme_and_artwork();
        let mut stack = shown_as(&cargo, at, source, (scale, theme));
        stack.paint(compositor, theme, &mut artwork);
        let shift = self.modifiers().shift;
        let issued = self.drag.issued;
        self.drag.carried = Some(Carried {
            cargo,
            stack,
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
    }

    /// Carry the drag through one pointer `event`.
    ///
    /// A motion answers where to, for the caller to resolve what is there
    /// ([`drag_over`](Self::drag_over)), and what is carried follows it once
    /// the batch is taken ([`settle`](Self::settle)); the press coming up drops
    /// the drag where it is, and any other press ends it with nothing dropped.
    pub fn drag_pointer(&mut self, compositor: &mut Compositor, event: &InputEvent) -> DragStep {
        if self.drag.carried.is_none() {
            return DragStep::Held;
        }
        match *event {
            InputEvent::PointerMoved { to } => {
                self.track_pointer(to, compositor);
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
    pub fn drag_items(&self) -> Option<(u64, &DragItems)> {
        match &self.drag.carried.as_ref()?.cargo {
            Cargo::Items { source, items } => Some((*source, &**items)),
            Cargo::Save { .. } | Cargo::Desktop { .. } => None,
        }
    }

    /// The window-channel window a carried save is for, which it is never
    /// dropped on.
    #[must_use]
    pub fn save_source(&self) -> Option<u64> {
        match self.drag.carried.as_ref()?.cargo {
            Cargo::Save { requester, .. } => Some(requester),
            Cargo::Items { .. } | Cargo::Desktop { .. } => None,
        }
    }

    /// Whether the carried drag is of icons carried off the desktop.
    #[must_use]
    pub fn carries_desktop_icons(&self) -> bool {
        self.drag
            .carried
            .as_ref()
            .is_some_and(|carried| matches!(carried.cargo, Cargo::Desktop { .. }))
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
    /// [`take_drag_report`](Self::take_drag_report). The desktop's folder
    /// resolved again from its listing — re-sorted, or settled in a new
    /// layout — owes no report: the application is told the same folder.
    pub fn drag_over(&mut self, compositor: &mut Compositor, next: DragSurface) {
        let Some(carried) = self.drag.carried.as_mut() else {
            return;
        };
        let arrived = !carried.over.same_place(&next);
        let changed = !next.reports_as(&carried.over);
        let left_reported = arrived && carried.over.reported();
        let saving = matches!(carried.cargo, Cargo::Save { .. });
        let icons = matches!(carried.cargo, Cargo::Desktop { .. });
        // The desktop takes a save itself, so arriving there accepts it at
        // once and owes no application anything; the report owed only relights
        // its folder. Its own icons brought back are put down there.
        carried.owed |= next.reported() && changed || left_reported;
        if arrived {
            carried.shown = match next {
                DragSurface::Desktop { .. } if saving => Some(DropOperation::Copy),
                DragSurface::Desktop { .. } if icons => Some(DropOperation::Move),
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
            Cargo::Desktop { drag, offered, .. } => {
                let over = match carried.over {
                    DragSurface::Offer { window_id, x, y } => Some((window_id, x, y)),
                    _ => None,
                };
                let left = offered
                    .take_if(|(window, _)| over.is_none_or(|(now, ..)| now != *window))
                    .map(|(window_id, serial)| ItemsOfferEnd {
                        window_id,
                        serial,
                        dropped: None,
                    });
                let over = over.map(|(window_id, x, y)| {
                    carried.serial = carried.serial.wrapping_add(1);
                    *offered = Some((window_id, carried.serial));
                    ItemsOffer {
                        window_id,
                        drag: *drag,
                        serial: carried.serial,
                        x,
                        y,
                        shift: carried.shift,
                    }
                });
                return Some(OwedReport::Desktop(DesktopReport { left, over }));
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

    /// Take the answer window `window_id`'s application gave the offer of
    /// icons carried off the desktop numbered `serial`: shown on the pointer
    /// while they are still over that window and the offer is the one
    /// standing there, and answering whether it was.
    pub fn items_offer_answered(
        &mut self,
        compositor: &mut Compositor,
        window_id: u64,
        serial: u32,
        verdict: Option<DropOperation>,
    ) -> bool {
        let Some(carried) = self.drag.carried.as_mut() else {
            return false;
        };
        let current = matches!(carried.cargo, Cargo::Desktop { .. })
            && matches!(carried.over, DragSurface::Offer { window_id: over, .. } if over == window_id)
            && carried.reported_here(serial);
        if !current {
            return false;
        }
        carried.shown = verdict;
        carried.answered = true;
        self.hold_drag_cursor(compositor);
        true
    }

    /// The items of the drag off the desktop numbered `drag`, for window
    /// `window_id`: while an offer of them stands there, or once they were
    /// dropped there.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for any other drag or window.
    pub fn offered_items(&self, window_id: u64, drag: u32) -> Result<&[DesktopItem], Errno> {
        match self.drag.carried.as_ref().map(|carried| &carried.cargo) {
            Some(Cargo::Desktop {
                drag: carried,
                cargo,
                offered: Some((offered, _)),
            }) if (*carried, *offered) == (drag, window_id) => Ok(&cargo.items),
            _ => match &self.drag.dropped {
                Some(dropped) if (dropped.window_id, dropped.drag) == (window_id, drag) => {
                    Ok(&dropped.items)
                }
                _ => Err(Errno::NotFound),
            },
        }
    }

    /// Item `index` of [`offered_items`](Self::offered_items), as the window
    /// reads it.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] past the last item, or as
    /// [`offered_items`](Self::offered_items).
    pub fn offered_item(
        &self,
        window_id: u64,
        drag: u32,
        index: u32,
    ) -> Result<OfferedItem<'_>, Errno> {
        let index = usize::try_from(index).map_err(|_| Errno::NotFound)?;
        let item = self
            .offered_items(window_id, drag)?
            .get(index)
            .ok_or(Errno::NotFound)?;
        Ok(OfferedItem {
            path: &item.path,
            id: item.id,
        })
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
        self.drag
            .dropped
            .take_if(|dropped| dropped.window_id == window);
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
            Cargo::Desktop { offered, .. } => {
                if offered.is_some_and(|(offered, _)| offered == window) {
                    *offered = None;
                }
                false
            }
        };
        if ends {
            return match self.end_drag(compositor, false)? {
                DragEnd::Save(end) => end.offer,
                DragEnd::Items(_) | DragEnd::Desktop(_) => None,
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

    /// Paint afresh, where they are, the parts of what the carried drag shows
    /// whose picture `landed` moved.
    pub(crate) fn repaint_drag(&mut self, compositor: &mut Compositor, landed: &Landed) {
        let Some(mut carried) = self.drag.carried.take() else {
            return;
        };
        let (theme, mut artwork) = self.theme_and_artwork();
        carried
            .stack
            .paint_landed(compositor, theme, (&mut artwork, landed));
        self.drag.carried = Some(carried);
    }

    /// Lay what the carried drag shows out afresh and paint it, at rest under
    /// the pointer: the theme, the scale or the fonts it is laid out by moved.
    pub fn relay_drag(&mut self, compositor: &mut Compositor) {
        let Some(mut carried) = self.drag.carried.take() else {
            return;
        };
        carried.stack.close(compositor);
        let at = self.router().pointer();
        let scale = compositor.scale();
        let (theme, mut artwork) = self.theme_and_artwork();
        carried.stack = shown_as(&carried.cargo, at, None, (scale, theme));
        carried.stack.paint(compositor, theme, &mut artwork);
        self.drag.carried = Some(carried);
    }

    /// Bring what the carried drag shows to the pointer once a batch of input
    /// has been taken: what rests on it moves with this batch, and what
    /// travels after it sets out at the next frame.
    pub(crate) fn settle_drag(&mut self, compositor: &mut Compositor) {
        let at = self.router().pointer();
        if let Some(carried) = self.drag.carried.as_mut() {
            carried.stack.follow(at, compositor);
        }
    }

    /// Move what the carried drag shows through its travel at `now_ns`.
    pub fn advance_drag(&mut self, now_ns: u64, compositor: &mut Compositor) {
        let at = self.router().pointer();
        if let Some(carried) = self.drag.carried.as_mut() {
            carried.stack.advance(now_ns, at, compositor);
        }
    }

    /// Fold the next frame what the carried drag shows owes, on a display
    /// refreshing at `refresh`, into `park_ns`; nothing while it is at rest.
    #[must_use]
    pub fn drag_park_deadline_ns(&self, now_ns: u64, park_ns: u64, refresh: RefreshRate) -> u64 {
        let due = self
            .drag
            .carried
            .as_ref()
            .and_then(|carried| carried.stack.next_frame_in(now_ns, refresh));
        park_within(park_ns, due)
    }

    /// The windows showing what the carried drag carries, the front first.
    pub fn drag_windows(&self) -> impl Iterator<Item = WindowId> + '_ {
        self.drag
            .carried
            .iter()
            .flat_map(|carried| carried.stack.windows())
    }

    /// Stop carrying, answering how it ended: where the pointer is if
    /// `dropped` and what is there takes the drag — a slot's application, or a
    /// place whose last answer the pointer shows — and otherwise on nothing.
    /// The slot it lit is latched dark for the caller's settle to paint.
    pub fn end_drag(&mut self, compositor: &mut Compositor, dropped: bool) -> Option<DragEnd> {
        let mut carried = self.drag.carried.take()?;
        self.drag.issued = carried.serial;
        carried.stack.close(compositor);
        let scale = compositor.scale();
        self.session_mut().taskbar_mut().set_drop_slot(None, scale);
        self.cursor_mut().hold(None);
        self.refresh_cursor(compositor);
        let source = match carried.cargo {
            Cargo::Items { source, .. } => source,
            Cargo::Save { pick, offered, .. } => {
                return Some(DragEnd::Save(save_end(
                    pick,
                    dropped,
                    &carried.over,
                    (carried.shown, carried.answered),
                    offered,
                )));
            }
            Cargo::Desktop {
                drag,
                cargo,
                offered,
            } => {
                let DesktopCargo { items, .. } = *cargo;
                let pointer = self.router().pointer();
                let over = (&carried.over, carried.shown);
                let end = desktop_end(dropped, over, offered, pointer);
                if end.dropped == DesktopDrop::Window {
                    if let Some(offer) = end.offer {
                        self.drag.dropped = Some(Dropped {
                            window_id: offer.window_id,
                            drag,
                            items,
                        });
                    }
                }
                return Some(DragEnd::Desktop(end));
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

/// How a save carried for `pick` ended over `over`, showing `shown` and
/// whether the place had answered, the offer `offered` standing: dropped on the
/// desktop, it goes in the folder there; on a window whose application accepted
/// the offer standing, in the folder it names; on one yet to answer, nowhere,
/// for the panel to say so; and anywhere else, nowhere.
fn save_end(
    pick: PickId,
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
    SaveEnd {
        pick,
        dropped,
        offer,
    }
}

/// How icons carried off the desktop ended over `over`, showing `shown`, the
/// offer `offered` standing and the pointer at `pointer`: dropped on a window
/// whose application took the offer standing there, that application performs
/// the drop; back on the desktop, they are put down there; and anywhere else,
/// they stand where they stood.
fn desktop_end(
    dropped: bool,
    (over, shown): (&DragSurface, Option<DropOperation>),
    offered: Option<(u64, u32)>,
    pointer: Point,
) -> DesktopEnd {
    let (dropped, at) = match (dropped, over, shown, offered) {
        (true, DragSurface::Desktop { .. }, _, _) => (DesktopDrop::Desktop(pointer), None),
        (true, &DragSurface::Offer { window_id, x, y }, Some(operation), Some((offer, _)))
            if offer == window_id =>
        {
            (DesktopDrop::Window, Some(OfferDrop { x, y, operation }))
        }
        _ => (DesktopDrop::Nothing, None),
    };
    let offer = offered.map(|(window_id, serial)| ItemsOfferEnd {
        window_id,
        serial,
        dropped: at,
    });
    DesktopEnd { dropped, offer }
}

/// The label the one item of an application's drag shows: its name less the
/// ending its listing hides, shown as a name from elsewhere is.
fn item_label(items: &DragItems) -> String {
    shown(items.shown_name()).into_owned()
}

/// What `cargo` is shown as with the pointer at `pointer`, carried out of a
/// window whose client area is `source` on the screen: one item as its tile,
/// held where it was pressed — an application's in the state its listing marks
/// it carried in — and several as a stack of their pictures, each setting out
/// from where it stood within that window, or from its rest without one.
fn shown_as(
    cargo: &Cargo,
    pointer: Point,
    source: Option<Rect>,
    (scale, theme): (tairix_geometry::Scale, &tairix_theme::Theme),
) -> Stack {
    let listed = {
        let metrics = Tiles::Large.metrics(scale, theme);
        let cell = Rect::new(0, 0, metrics.cell_width, metrics.cell_height);
        Tiles::Large.layout().icon_side(cell, scale, theme)
    };
    let items = match cargo {
        Cargo::Items { items, .. } => Ok(&**items),
        Cargo::Desktop { cargo, .. } => Ok(&cargo.shown),
        Cargo::Save { document, .. } => Err(document),
    };
    match items {
        Ok(items) => {
            let selected = ControlState {
                selection: SelectionState::Selected,
                ..ControlState::idle()
            };
            let one = items.count() == 1;
            let looks = items.pictures().enumerate().map(|(index, picture)| {
                let look = Look {
                    label: one.then(|| item_label(items)),
                    icon: IconKind::for_asset(picture.icon.as_str()),
                    own: picture.own,
                    state: if one { selected } else { ControlState::idle() },
                    count: (!one && index == 0).then_some(items.count()),
                };
                let stood = picture
                    .from
                    .zip(source)
                    .map(|(from, source)| stood_at(pointer, from, source));
                (look, stood)
            });
            let grab = items.pictures().next().and_then(|lead| lead.from).map_or(
                PictureGrab::CENTRE,
                |(dx, dy)| PictureGrab {
                    offset: (dx.saturating_neg(), dy.saturating_neg()),
                    side: listed,
                },
            );
            Stack::new(looks, pointer, grab, (scale, theme))
        }
        Err(document) => {
            let look = Look {
                label: Some(shown(&document.name).into_owned()),
                icon: document.icon,
                own: None,
                state: ControlState::idle(),
                count: None,
            };
            Stack::new([(look, None)], pointer, document.grab, (scale, theme))
        }
    }
}

/// Where a picture whose centre stood `from` the pointer at `pointer` stood
/// on the screen, kept within `source`, the window it was carried out of.
fn stood_at(pointer: Point, from: (i32, i32), source: Rect) -> Point {
    let at = Point::new(
        pointer.x.saturating_add(from.0),
        pointer.y.saturating_add(from.1),
    );
    if source.is_empty() {
        return pointer;
    }
    let last = |start: i32, extent: u32| {
        start.saturating_add(i32::try_from(extent.saturating_sub(1)).unwrap_or(i32::MAX))
    };
    Point::new(
        at.x.clamp(source.left(), last(source.left(), source.width)),
        at.y.clamp(source.top(), last(source.top(), source.height)),
    )
}

#[cfg(test)]
#[path = "drag_tests.rs"]
mod tests;

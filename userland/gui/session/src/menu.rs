//! The desktop's one menu chain: the seat's singleton, and the only thing on
//! the system that renders a menu (`plans/NEW-MENUS.md` §1).
//!
//! A chain is a root plate and the descendants open beneath it. A **plate** is
//! a title band over a column of rows: the shared [`TitleBar`] seating no
//! commands, and the shared [`Menu`]. A **child** is a submenu — more rows from
//! the same model — or one of the two surfaces the session draws itself: the
//! information panel, and the quick-entry field that owns the keyboard and
//! answers with the text it was given. All three hang where a submenu hangs and
//! die with the chain. A row may carry a command *and* a child, so clicking it
//! answers while arriving on it opens.
//!
//! The application describes and the desktop decides. A client hands over a
//! model and an anchor; everything after that — titling, placement, drawing,
//! the grab, routing, traversal, dismissal, and the one outcome — is the
//! session's. Nothing a client sends pins a chain open, no client draws a plate
//! pixel, and no client learns where the pointer is inside one.
//!
//! Nothing here touches the compositor. The chain owns the model, the state,
//! and the geometry; the session presents [`surfaces`](MenuChain::surfaces)
//! and tears down what the chain no longer lists. That is what lets every rule
//! in section 1 be tested without a screen.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use tairix_abi::window_ipc::{AppMenuItemId, MenuRefusal, APP_MENU_ENTRY_MAX};
use tairix_controls::damage::{self, Repaint};
use tairix_controls::{
    plate_rect, AboutSheet, ChainChild, ChainModel, ClipboardChord, Menu, MenuAction,
    PlatePlacement, RowIcon, TextAction, TextField, TitleBar, TitleBarEvent, ABOUT_ICON_SIDE,
};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_taskbar::MenuSubject;

use tairix_icon::{ArtworkCache, ArtworkResolver, IconKind, IconPicture, IconRequest};

use crate::clipboard::TextClipboard;
use crate::windows::seat_menu_refusal;
use tairix_theme::Theme;
use tairix_wm::{ChromeEpoch, InputEvent, Key, Modifiers, NamedKey};

/// Who asked for the chain, and therefore where its one answer goes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChainOwner {
    /// An application's window. The answer is the `MenuClosed` naming
    /// `open_id` that the engine holds it to.
    Window {
        /// The window-channel id of the window the chain is scoped by.
        window_id: u64,
        /// The session-minted id of the open this chain answers.
        open_id: u64,
    },
    /// The desktop's own pinboard backdrop. The answer is resolved in
    /// process against the desktop model.
    Backdrop,
    /// The desktop's own icon bar. The answer is resolved in process against
    /// the bar's own subject, which is what a chosen row of it acts on.
    ///
    /// The subject travels with the address rather than beside it, so a chain
    /// the next open displaces cannot have its answer read against the next
    /// chain's subject.
    Bar(MenuSubject),
}

/// How a chain ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChainOutcome {
    /// A row was chosen.
    Chosen(AppMenuItemId),
    /// A row's quick-entry field was committed, carrying the field's own id
    /// and the text the user typed.
    ///
    /// Its own outcome rather than a [`Chosen`](Self::Chosen), because a row
    /// may be chooseable *and* carry a field: the two ids are what tell one
    /// from the other without the owner inferring anything.
    Entered(AppMenuItemId, String),
    /// The chain closed without a choice.
    Dismissed,
    /// No chain was brought up at all, for a reason about the **seat** rather
    /// than about the request: a malformed or unauthorised ask is refused by
    /// the open call itself and mints nothing.
    Refused(MenuRefusal),
}

/// What routing one event into the chain asks the session to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChainAction {
    /// The event was consumed and nothing changed.
    Consumed,
    /// The chain's geometry or pixels changed; re-present its surfaces.
    Redraw,
    /// The chain closed.
    ///
    /// Neither the answer nor the surfaces are here. The answer leaves through
    /// [`MenuChain::take_answers`], so the one delivery point cannot be
    /// bypassed and no path can answer a chain twice; the surfaces go by the
    /// session reconciling against [`MenuChain::surfaces`], which a closed
    /// chain reports empty.
    Closed,
}

/// One picture of the open chain that is wanted, and the pixel side it must
/// be rasterised at.
///
/// A row or the information sheet *names* where its picture comes from;
/// resolving it is the session's, because only the session owns an icon cache
/// and a sandboxed decoder. A paint reads nothing, so the answer is set before
/// the next one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChainIconWant {
    /// What wants it.
    pub target: ChainPicture,
    /// The side, in physical pixels, the picture must be rasterised at.
    pub side: u32,
    /// Where the picture comes from.
    pub source: RowIcon,
}

/// A picture the open chain draws.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ChainPicture {
    /// Model row `row`'s, as [`MenuChain::set_row_artwork`] names it.
    Row(usize),
    /// The open information sheet's, as [`MenuChain::set_info_artwork`]
    /// sets it.
    Info,
}

/// One surface the chain occupies on screen.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChainSurface {
    /// Where it sits, in screen pixels.
    pub rect: Rect,
    /// What it is.
    pub kind: SurfaceKind,
    /// What of it the session has still to paint.
    pub repaint: Repaint,
}

/// Which of the chain's surfaces a rectangle is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SurfaceKind {
    /// The plate at this depth, root first.
    Plate(usize),
    /// The session-drawn sheet about the application.
    Info,
    /// The session-drawn quick-entry field.
    Entry,
}

/// One plate of the open chain.
#[derive(Clone, Debug)]
struct Plate {
    /// The model rows this plate shows, in order.
    rows: Vec<usize>,
    /// The shared renderer over those rows.
    menu: Menu,
    /// The band, seating no commands.
    band: TitleBar,
    /// Where the plate sits, in screen pixels.
    rect: Rect,
    /// Whether a drag pinned it: its placement has stopped being derived from
    /// the anchor, because the user put it where it is.
    pinned: bool,
    /// The row of this plate whose child is open, if any.
    open_row: Option<usize>,
    /// What of the plate the session has still to paint.
    repaint: Repaint,
}

/// The desktop's own sheet about the application, hanging where a submenu's
/// plate would and closing with the chain.
#[derive(Clone, Debug)]
struct InfoPanel {
    /// What it states, read from the owner's signed manifest by the host.
    sheet: Box<AboutSheet>,
    /// Where it sits.
    rect: Rect,
    /// What of the sheet the session has still to paint. It states facts that
    /// never change, so after its first paint it owes nothing until its
    /// picture arrives.
    repaint: Repaint,
}

/// Logical width of the quick-entry surface at the reference density: wide
/// enough for a filesystem name at a glance without dwarfing the plate it
/// hangs off.
const ENTRY_SURFACE_WIDTH: u32 = 240;

/// The desktop's own quick-entry surface, hanging where a submenu's plate
/// would.
///
/// The field is the session's, so an application never sees a keystroke inside
/// desktop chrome: it declares that a field exists and what it starts out
/// holding, and the one answer it gets back is the committed text.
#[derive(Clone, Debug)]
struct EntrySurface {
    /// The id a commit answers with — the field's own, never its row's.
    id: AppMenuItemId,
    /// The band, seating no commands, titled from the row the field hangs
    /// off so the surface reads as part of the chain.
    band: TitleBar,
    /// The field itself.
    field: TextField,
    /// Where it sits, in screen pixels.
    rect: Rect,
    /// What of the surface the session has still to paint.
    repaint: Repaint,
}

impl EntrySurface {
    /// Where the field sits on screen.
    fn field_rect(&self, geom: &ChainGeometry<'_>) -> Rect {
        let band_h = TitleBar::band_height(geom.scale, geom.theme).min(self.rect.height);
        entry_field_rect(self.rect, band_h, geom)
    }

    /// Owe the screen rectangles in `damage`, in the surface's own pixels,
    /// answering what the chain must do about them.
    fn owe(&mut self, damage: &Region) -> ChainAction {
        let origin = self.rect.origin;
        for rect in damage.rects() {
            self.repaint.add(Rect::new(
                rect.left().saturating_sub(origin.x),
                rect.top().saturating_sub(origin.y),
                rect.width,
                rect.height,
            ));
        }
        if damage.is_empty() {
            ChainAction::Consumed
        } else {
            ChainAction::Redraw
        }
    }
}

/// How long the pointer may rest on a sibling row it crossed on its way to an
/// open child before the crossing counts as choosing that row.
pub const SUBMENU_SETTLE_NS: u64 = 300 * tairix_abi::time::NANOS_PER_MILLI;

/// How far, in logical pixels, a path may wander from where it left the open
/// row before it has a heading: a move that small is the hand settling, not a
/// direction.
const AIM_SLACK: u32 = 4;

/// A crossing of a sibling row, deferred because the pointer was heading into
/// the open child: it settles onto `row` at `due_ns` unless the pointer reaches
/// the child or turns away first.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Settle {
    /// The plate whose row was crossed.
    depth: usize,
    /// The row under the pointer.
    row: usize,
    /// When the crossing becomes a choice.
    due_ns: u64,
}

/// A drag in flight: which plate the band press landed on, and where the
/// pointer was when the plate last moved.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Drag {
    /// The plate depth being dragged.
    plate: usize,
    /// The pointer position the last move was measured from.
    from: Point,
}

/// The scale and theme every geometry answer is resolved at, with the screen
/// the chain must stay inside.
#[derive(Copy, Clone, Debug)]
pub struct ChainGeometry<'a> {
    /// The seat's screen rectangle.
    pub screen: Rect,
    /// The output's density.
    pub scale: Scale,
    /// The active theme.
    pub theme: &'a Theme,
    /// The mode the output is drawn at, which a chain is placed against.
    pub epoch: ChromeEpoch,
}

impl ChainGeometry<'_> {
    /// The mode a chain records so it can tell when the ground has moved
    /// under it.
    const fn mode(&self) -> (Rect, ChromeEpoch) {
        (self.screen, self.epoch)
    }
}

/// The seat's one open chain.
///
/// One per seat, owned by the session: the open chain is a single piece of
/// seat state and not a set, so opening a menu closes whatever was up and
/// answers its requester.
#[derive(Debug, Default)]
pub struct MenuChain {
    open: Option<OpenChain>,
    /// Answers owed to chains that have closed, in the order they closed.
    ///
    /// A chain very often ends where its answer cannot be delivered — inside
    /// the window engine's own serve pass, which already holds the borrow
    /// `deliver_event` needs. Queueing them all rather than only those is what
    /// makes the delivery point single, so no close can answer twice and none
    /// can answer not at all.
    answers: Vec<(ChainOwner, ChainOutcome)>,
}

/// Everything one open chain is.
#[derive(Clone, Debug)]
struct OpenChain {
    owner: ChainOwner,
    model: ChainModel,
    plates: Vec<Plate>,
    /// The information panel hanging off the deepest plate, if one is open.
    info: Option<InfoPanel>,
    /// The quick-entry surface hanging off the deepest plate, if one is open.
    ///
    /// Its own field rather than a variant beside `info`, because the two
    /// differ in what they *are* — one states facts and claims a press
    /// harmlessly, the other owns the keyboard and answers — and a chain
    /// never has both: opening either closes whatever the plate had.
    entry: Option<EntrySurface>,
    /// Where the root plate opens, in screen pixels.
    placement: PlatePlacement,
    drag: Option<Drag>,
    /// The pointer's previous position: a sample that has not moved decides
    /// nothing.
    last_pointer: Option<Point>,
    /// Where the pointer last stood on the row whose child is open, or where
    /// it was when that child opened: the apex a path aimed at the child is
    /// measured from. It stays put while the path crosses the rows beside the
    /// open one, so a slow or stepped path is judged by where it is heading
    /// rather than by its last step.
    aim_apex: Option<Point>,
    /// A sibling crossing waiting to become a choice.
    settle: Option<Settle>,
    /// The plate whose open row has lent its highlight to the row the pointer
    /// rests on, one that can open nothing, so its Authority Mark can be
    /// read. The open row takes it back once the pointer is anywhere else.
    lent: Option<usize>,
    /// The answer this chain has settled on, once something has ended it.
    outcome: Option<ChainOutcome>,
    /// The screen and output mode the chain was placed against.
    mode: (Rect, ChromeEpoch),
    /// Whether a frame carrying this chain has reached the display, so the
    /// announcement is made once per open. Per-open rather than per-seat
    /// because a fresh chain is a fresh `OpenChain`: nothing has to remember
    /// to clear it.
    shown: bool,
}

impl MenuChain {
    /// A seat with no chain up.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            open: None,
            answers: Vec::new(),
        }
    }

    /// Take the answers owed to chains that have closed since the last call.
    ///
    /// The session's one delivery point: an application's chain is answered
    /// with the `MenuClosed` the engine holds it to, and the desktop's own is
    /// answered in-process.
    pub fn take_answers(&mut self) -> Vec<(ChainOwner, ChainOutcome)> {
        core::mem::take(&mut self.answers)
    }

    /// Report who owns the open chain if the frame just handed to the display
    /// is the first to carry it.
    ///
    /// Called immediately after a frame reached the display, which is what
    /// makes the claim true — the chain's surfaces are compositor windows, so
    /// a frame composited while it is up carries its plates. Once per open,
    /// and never for a chain that has closed: a chain the session could not
    /// draw is refused rather than announced.
    ///
    /// Takes a reporter rather than returning anything so an idle wake — which
    /// is nearly every wake — costs a bool test.
    pub fn report_newly_shown(&mut self, report: impl FnOnce(&ChainOwner)) {
        let Some(chain) = self.open.as_mut() else {
            return;
        };
        if chain.shown {
            return;
        }
        chain.shown = true;
        report(&chain.owner);
    }

    /// Whether a chain is up. While one is, it holds the seat's pointer and
    /// keyboard.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// The owner of the open chain, if one is up.
    ///
    /// Borrowed rather than cloned: an owner carries the bar's subject, which
    /// holds a catalog id, and this is read on the pointer-sample path.
    #[must_use]
    pub fn owner(&self) -> Option<&ChainOwner> {
        self.open.as_ref().map(|chain| &chain.owner)
    }

    /// The window-channel id of the open chain's owning window, for a chain an
    /// application asked for; `None` for one the desktop opened for itself.
    #[must_use]
    pub fn owner_window(&self) -> Option<u64> {
        match self.owner()? {
            ChainOwner::Window { window_id, .. } => Some(*window_id),
            ChainOwner::Backdrop | ChainOwner::Bar(_) => None,
        }
    }

    /// Open `model` for `owner`, anchored at `anchor` in screen pixels and
    /// opening on `side` with `gap` pixels of clearance.
    ///
    /// The chain that was up, if any, is closed and its owner queued a
    /// `Dismissed`: the chain is the seat's singleton, so a second open is an
    /// end to the first rather than a second menu beside it. A model with no
    /// rows on its root plate opens nothing and is refused, because a plate
    /// with nothing to choose is a surface the user cannot leave by choosing.
    ///
    /// The surfaces of the displaced chain are the caller's to take down,
    /// which it does by reconciling against [`surfaces`](Self::surfaces).
    ///
    /// # Errors
    ///
    /// [`ModelRefused`] for a model that cannot be shown; the chain that was
    /// up is left exactly as it was.
    pub fn open(
        &mut self,
        owner: ChainOwner,
        mut model: ChainModel,
        placement: PlatePlacement,
        geom: &ChainGeometry<'_>,
    ) -> Result<(), ModelRefused> {
        // The chain ends when the scale or theme changes, so the side every
        // plate draws its glyphs at is fixed for the chain's whole life.
        model.resolve_glyphs(geom.scale, geom.theme);
        let root = plate_for(&model, None)?;
        self.close_open();
        let mut chain = OpenChain {
            owner,
            model,
            plates: alloc::vec![root],
            info: None,
            entry: None,
            placement,
            drag: None,
            last_pointer: None,
            aim_apex: None,
            settle: None,
            lent: None,
            outcome: None,
            mode: geom.mode(),
            shown: false,
        };
        chain.replace_geometry(geom);
        self.open = Some(chain);
        Ok(())
    }

    /// End the chain that is up, queueing its owner a `Dismissed`.
    fn close_open(&mut self) -> bool {
        let Some(chain) = self.open.take() else {
            return false;
        };
        self.answers.push((chain.owner, ChainOutcome::Dismissed));
        true
    }

    /// Every surface the chain occupies, root plate first and the attached
    /// window (if any) last.
    ///
    /// The session presents exactly this list and tears down anything it holds
    /// that the list no longer names, so a surface can never outlive the state
    /// that placed it.
    #[must_use]
    pub fn surfaces(&self) -> Vec<ChainSurface> {
        let Some(chain) = self.open.as_ref() else {
            return Vec::new();
        };
        let mut out: Vec<ChainSurface> = chain
            .plates
            .iter()
            .enumerate()
            .map(|(depth, plate)| ChainSurface {
                rect: plate.rect,
                kind: SurfaceKind::Plate(depth),
                repaint: plate.repaint.clone(),
            })
            .collect();
        if let Some(panel) = chain.info.as_ref() {
            out.push(ChainSurface {
                rect: panel.rect,
                kind: SurfaceKind::Info,
                repaint: panel.repaint.clone(),
            });
        }
        if let Some(entry) = chain.entry.as_ref() {
            out.push(ChainSurface {
                rect: entry.rect,
                kind: SurfaceKind::Entry,
                repaint: entry.repaint.clone(),
            });
        }
        out
    }

    /// Record that the surface `kind` now carries what the chain last asked
    /// for, so what it owes from here is only what changes after this.
    ///
    /// Called per surface the session actually painted. A surface the heap
    /// refused keeps what it owes and is painted on the next pass instead, so
    /// a refusal can never leave stale pixels reported as current.
    pub fn presented(&mut self, kind: SurfaceKind) {
        let Some(chain) = self.open.as_mut() else {
            return;
        };
        let owed = match kind {
            SurfaceKind::Plate(depth) => {
                chain.plates.get_mut(depth).map(|plate| &mut plate.repaint)
            }
            SurfaceKind::Info => chain.info.as_mut().map(|panel| &mut panel.repaint),
            SurfaceKind::Entry => chain.entry.as_mut().map(|entry| &mut entry.repaint),
        };
        if let Some(owed) = owed {
            *owed = Repaint::clean();
        }
    }

    /// Every row of the open chain that names where its picture comes from,
    /// and the side each must be rasterised at.
    ///
    /// Only rows on a plate that is actually up are reported, so a submenu
    /// nobody opened decodes nothing, and a row already holding a picture of
    /// the right side is left out, so asking before each present costs a
    /// comparison rather than a decode.
    #[must_use]
    pub fn icon_wants(&self, geom: &ChainGeometry<'_>) -> Vec<ChainIconWant> {
        let Some(chain) = self.open.as_ref() else {
            return Vec::new();
        };
        let side = Menu::icon_side(geom.scale, geom.theme);
        let mut wants = Vec::new();
        for plate in &chain.plates {
            for &row in &plate.rows {
                let Some(entry) = chain.model.rows().get(row) else {
                    continue;
                };
                let Some(source) = entry.icon_source() else {
                    continue;
                };
                if entry
                    .drawn()
                    .artwork()
                    .is_some_and(|art| art.width() == side)
                {
                    continue;
                }
                wants.push(ChainIconWant {
                    target: ChainPicture::Row(row),
                    side,
                    source: source.clone(),
                });
            }
        }
        if let Some(info) = chain.info.as_ref() {
            let side = geom.scale.scale_length(ABOUT_ICON_SIDE);
            let pictured = info.sheet.artwork().is_some_and(|art| art.width() == side);
            if let (Some(source), false) = (info.sheet.source(), pictured) {
                wants.push(ChainIconWant {
                    target: ChainPicture::Info,
                    side,
                    source: source.clone(),
                });
            }
        }
        wants
    }

    /// Give model row `row` the picture the session resolved for it, or take
    /// the one it had when nothing resolved.
    ///
    /// Set on the model *and* on whichever open plate draws that row: the
    /// model is what a plate rebuilt at the same depth is built from, so a
    /// picture survives a submenu closing and reopening without a second
    /// decode. Borrowed rather than owned: the picture is copied into the
    /// model and into each plate that draws the row, so taking it by value
    /// would buy a third copy the caller then drops. A row with no picture
    /// that still resolves none owes no repaint, so asking before every
    /// present costs nothing while its decode is in flight.
    pub fn set_row_artwork(&mut self, row: usize, art: Option<&tairix_raster::Surface>) {
        let Some(chain) = self.open.as_mut() else {
            return;
        };
        if let Some(entry) = chain.model.rows_mut().get_mut(row) {
            match art {
                Some(art) => {
                    *entry.drawn_mut() = entry.drawn().clone().with_artwork(art.clone());
                }
                None if entry.drawn().artwork().is_some() => {
                    *entry.drawn_mut() = entry.drawn().clone().without_artwork();
                }
                None => {}
            }
        }
        for plate in &mut chain.plates {
            let Some(at) = plate.rows.iter().position(|&held| held == row) else {
                continue;
            };
            let Some(item) = plate.menu.items_mut().get_mut(at) else {
                continue;
            };
            if art.is_none() && item.artwork().is_none() {
                continue;
            }
            let drawn = item.clone();
            *item = match art {
                Some(art) => drawn.with_artwork(art.clone()),
                None => drawn.without_artwork(),
            };
            plate.repaint = Repaint::Whole;
        }
    }

    /// Give the information sheet the picture the session resolved for it:
    /// on the model too, so a sheet opened again draws it at once. A sheet
    /// whose picture is still in flight keeps its glyph and owes nothing.
    pub fn set_info_artwork(&mut self, art: Option<&tairix_raster::Surface>) {
        let (Some(chain), Some(art)) = (self.open.as_mut(), art) else {
            return;
        };
        chain.model.set_info_artwork(art);
        if let Some(info) = chain.info.as_mut() {
            info.sheet.set_artwork(art.clone());
            info.repaint = Repaint::Whole;
        }
    }

    /// The screen rectangle row `row` of the plate at `depth` occupies.
    ///
    /// The forward mirror of the chain's own hit-testing, so a caller aiming
    /// *at* a row reads the rectangle the chain lays out rather than a
    /// re-derived guess. `None` for a depth or row the chain does not have.
    #[must_use]
    pub fn row_rect(&self, depth: usize, row: usize, geom: &ChainGeometry<'_>) -> Option<Rect> {
        row_rect(self.open.as_ref()?.plates.get(depth)?, row, geom)
    }

    /// The screen rectangle and text of the row the pointer rests on, when
    /// that row explains why it cannot be chosen.
    ///
    /// The chain places its own plates, so it answers in screen pixels. An
    /// explanation is shown as the seat's tip on dwell and is never drawn on
    /// the row: a caption beside every disabled label is what made a plate as
    /// wide as its longest excuse.
    #[must_use]
    pub fn hovered_tip(&self, geom: &ChainGeometry<'_>) -> Option<(Rect, &str)> {
        let chain = self.open.as_ref()?;
        // The deepest plate is the one the pointer is working in; an ancestor
        // keeps its highlight to show the path, not the pointer.
        let depth = chain.plates.len().checked_sub(1)?;
        let plate = chain.plates.get(depth)?;
        let index = plate.menu.current()?;
        let &model_row = plate.rows.get(index)?;
        let tip = chain.model.rows().get(model_row)?.tip()?;
        let rect = row_rect(plate, index, geom)?;
        Some((rect, tip))
    }

    /// Paint the surface `kind` into `surface`, whose extent is that
    /// surface's own rectangle.
    ///
    /// The one entry point for a chain's pixels, so the session supplies a
    /// surface and never a recipe. A caller repainting part of a surface
    /// clips `surface` to the rectangles it means to write and calls this:
    /// every pixel inside the clip is re-derived exactly as a whole paint
    /// would have laid it, so a partial repaint and a whole one cannot
    /// disagree.
    pub fn render_surface(
        &self,
        kind: SurfaceKind,
        surface: &mut tairix_raster::Surface,
        geom: &ChainGeometry<'_>,
    ) {
        match kind {
            SurfaceKind::Plate(depth) => self.render_plate(depth, surface, geom),
            SurfaceKind::Info => self.render_info(surface, geom),
            SurfaceKind::Entry => self.render_entry(surface, geom),
        }
    }

    /// Paint the plate at `depth`.
    ///
    /// The band and the rows are the two shared controls over one shared plate
    /// ground; nothing here is a second recipe for any of the three, and the
    /// rows take that ground rather than laying one of their own.
    fn render_plate(
        &self,
        depth: usize,
        surface: &mut tairix_raster::Surface,
        geom: &ChainGeometry<'_>,
    ) {
        let Some(chain) = self.open.as_ref() else {
            return;
        };
        let Some(plate) = chain.plates.get(depth) else {
            return;
        };
        let local = Rect::new(0, 0, plate.rect.width, plate.rect.height);
        let band_h = TitleBar::band_height(geom.scale, geom.theme).min(local.height);
        // The plate is laid first, its band's ground with it; the band draws
        // its title over that, and the rows take the ground as it is.
        lay_plate(surface, (local.width, local.height), band_h, geom);
        plate.band.render(
            surface,
            Rect::new(0, 0, local.width, band_h),
            geom.scale,
            geom.theme,
            None,
        );
        // Rows only: the ground and rim under them are the plate's, laid
        // once above, so a second plate here would rim and round the rows
        // inside the one they already sit on.
        plate
            .menu
            .render_rows(surface, local, band_h, geom.scale, geom.theme);
    }

    /// Paint the information panel: the attested facts on the same floating
    /// ground every plate stands on.
    fn render_info(&self, surface: &mut tairix_raster::Surface, geom: &ChainGeometry<'_>) {
        let Some(panel) = self.open.as_ref().and_then(|chain| chain.info.as_ref()) else {
            return;
        };
        let local = Rect::new(0, 0, panel.rect.width, panel.rect.height);
        let band_h = TitleBar::band_height(geom.scale, geom.theme).min(local.height);
        lay_plate(surface, (local.width, local.height), band_h, geom);
        panel.sheet.render(surface, local, geom.scale, geom.theme);
    }

    /// Paint the quick-entry surface: the band over the field, on the same
    /// floating ground every plate stands on.
    fn render_entry(&self, surface: &mut tairix_raster::Surface, geom: &ChainGeometry<'_>) {
        let Some(entry) = self.open.as_ref().and_then(|chain| chain.entry.as_ref()) else {
            return;
        };
        let local = Rect::new(0, 0, entry.rect.width, entry.rect.height);
        let band_h = TitleBar::band_height(geom.scale, geom.theme).min(local.height);
        lay_plate(surface, (local.width, local.height), band_h, geom);
        entry.band.render(
            surface,
            Rect::new(0, 0, local.width, band_h),
            geom.scale,
            geom.theme,
            None,
        );
        entry.field.render(
            surface,
            entry_field_rect(local, band_h, geom),
            geom.scale,
            geom.theme,
        );
    }

    /// Close the chain, answering its owner `Dismissed`.
    ///
    /// The seat calls this for every reason a chain ends that is not the user
    /// choosing a row: the owner died, the seat was lost, the output was
    /// resized, the scale or theme changed. Re-placing a plate the user has
    /// dragged is not defined, so a mode change under the gesture ends the
    /// chain rather than moving it.
    /// Returns whether a chain was up.
    pub fn dismiss(&mut self) -> bool {
        self.close_open()
    }

    /// Answer `owner` that no chain could be brought up, for a reason about
    /// the seat.
    ///
    /// The open was accepted, so the application is owed exactly one answer;
    /// this is it. Whatever chain is up stays up: a refusal is a fact about
    /// the seat at this instant, not a reason to take down a menu the user is
    /// already using.
    pub fn refuse(&mut self, owner: ChainOwner, reason: MenuRefusal) {
        self.answers.push((owner, ChainOutcome::Refused(reason)));
    }

    /// End the chain that is up because the session cannot give it a surface,
    /// answering `NoResources`. Returns whether one was up.
    ///
    /// The one place that reason is honest: a chain is refused for want of
    /// memory when the memory is actually refused, which is when its plate is
    /// drawn, not when it is asked for.
    pub fn exhausted(&mut self) -> bool {
        let Some(chain) = self.open.take() else {
            return false;
        };
        self.answers
            .push((chain.owner, ChainOutcome::Refused(MenuRefusal::NoResources)));
        true
    }

    /// Size the open chain's plates and information panel to their content
    /// again and place them, a plate the user dragged keeping its place: text
    /// they were measured with provisionally has landed. Answers whether a
    /// chain is up to present.
    pub fn refit(&mut self, geom: &ChainGeometry<'_>) -> bool {
        let Some(chain) = self.open.as_mut() else {
            return false;
        };
        chain.refit(geom);
        true
    }

    /// End the chain if the ground has moved under it: the seat's output
    /// resized, or its scale or theme switched. Returns whether it did.
    ///
    /// One rule the chain enforces on itself rather than a dismissal every
    /// mode-changing call site has to remember, because forgetting one leaves
    /// a plate placed against a screen that no longer exists. Re-placing is
    /// not the answer: a plate the user has dragged has a position that is
    /// theirs, and no rule can carry it onto a different screen.
    pub fn settle_mode(&mut self, geom: &ChainGeometry<'_>) -> bool {
        let moved = self
            .open
            .as_ref()
            .is_some_and(|chain| chain.mode != geom.mode());
        moved && self.close_open()
    }

    /// The park this seat needs, shortened to the moment a deferred crossing
    /// becomes a choice. No deferral, no deadline: a quiet chain wakes nothing.
    #[must_use]
    pub fn park_deadline_ns(&self, now_ns: u64, park_ns: u64) -> u64 {
        match self.open.as_ref().and_then(|chain| chain.settle) {
            Some(settle) => park_ns.min(settle.due_ns.saturating_sub(now_ns)),
            None => park_ns,
        }
    }

    /// Settle a deferred crossing that has come due at `now_ns`, answering
    /// whether the chain changed. A pointer at rest sends nothing, so this is
    /// what makes a rest on a crossed row choose it.
    pub fn tick(&mut self, now_ns: u64, geom: &ChainGeometry<'_>) -> bool {
        self.open
            .as_mut()
            .is_some_and(|chain| chain.settle_due(now_ns, geom))
    }

    /// Carry out a clipboard chord in the open quick-entry field, answering
    /// `None` when no field is open — the chord then routes as any other key.
    ///
    /// A cut takes the selection out only once the clipboard holds it, so a
    /// copy that fails never costs the user their text.
    pub fn entry_chord(
        &mut self,
        chord: ClipboardChord,
        clipboard: &mut dyn TextClipboard,
        geom: &ChainGeometry<'_>,
    ) -> Option<ChainAction> {
        let chain = self.open.as_mut()?;
        let entry = chain.entry.as_mut()?;
        let field = entry.field_rect(geom);
        let mut damage = damage::sink();
        match chord {
            ClipboardChord::Copy => {
                if let Some(text) = entry.field.selected_text() {
                    let _ = clipboard.hold_text(text);
                }
            }
            ClipboardChord::Cut => {
                let held = entry
                    .field
                    .selected_text()
                    .map(|text| clipboard.hold_text(text));
                if matches!(held, Some(Ok(()))) {
                    entry.field.delete_selection(field, &mut damage);
                }
            }
            ClipboardChord::Paste => {
                if let Some(text) = clipboard.text() {
                    entry.field.insert_text(text, field, &mut damage);
                }
            }
        }
        Some(entry.owe(&damage))
    }

    /// Close the chain if `window_id` owns it — the window closed, or the
    /// application behind it died. Returns whether it did.
    pub fn dismiss_owner(&mut self, window_id: u64) -> bool {
        owned_by(self.owner(), window_id) && self.close_open()
    }

    /// Route one seat event into the chain.
    ///
    /// While a chain is up the seat's pointer and keyboard are its own: every
    /// event routes here first and none of them reaches what is behind. A
    /// press outside the chain dismisses it and is **consumed** — a dismissal
    /// must not double as a click on whatever the menu was covering.
    pub fn handle(
        &mut self,
        event: &InputEvent,
        pointer: Point,
        geom: &ChainGeometry<'_>,
        now_ns: u64,
    ) -> ChainAction {
        let Some(chain) = self.open.as_mut() else {
            return ChainAction::Consumed;
        };
        let acted = match event {
            InputEvent::KeyPressed { key, modifiers } => {
                chain.settle = None;
                chain.last_pointer = Some(pointer);
                chain.on_key(*key, *modifiers, geom)
            }
            InputEvent::KeyReleased { .. } | InputEvent::ModifiersChanged { .. } => {
                ChainAction::Consumed
            }
            _ => chain.on_pointer(event, pointer, geom, now_ns),
        };
        if matches!(acted, ChainAction::Closed) {
            let outcome = chain_outcome(self.open.as_mut());
            if let Some(chain) = self.open.take() {
                self.answers.push((chain.owner, outcome));
            }
        }
        acted
    }
}

/// Resolve the pictures the open chain's rows asked for, through the
/// session's one artwork cache.
///
/// The identical request a taskbar slot makes for an application's icon, or a
/// file manager's tile for a class, so a row and that slot or tile draw the
/// same picture from the same cache. The artwork tier reads and validates a
/// bundle's own *signed* manifest, so nothing outside a bundle's declared icon
/// is ever read and an unresolvable path resolves to nothing — the row then
/// draws the built-in glyph rather than blanking.
pub fn resolve_chain_icons(
    chain: &mut MenuChain,
    geom: &ChainGeometry<'_>,
    resolver: &mut dyn ArtworkResolver,
    cache: &mut ArtworkCache,
) {
    for want in chain.icon_wants(geom) {
        let request = match &want.source {
            RowIcon::Bundle(dir) => IconRequest::bundle(IconKind::AppBundle, dir),
            RowIcon::Class(kind) => IconRequest::kind(*kind),
        };
        let art = cache
            .artwork(resolver, request, want.side)
            .and_then(IconPicture::artwork);
        match want.target {
            ChainPicture::Row(row) => chain.set_row_artwork(row, art),
            ChainPicture::Info => chain.set_info_artwork(art),
        }
    }
}

/// Whether `owner` is the window-channel window `window_id`.
fn owned_by(owner: Option<&ChainOwner>, window_id: u64) -> bool {
    matches!(owner, Some(ChainOwner::Window { window_id: id, .. }) if *id == window_id)
}

/// Why a model cannot be shown as a chain.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ModelRefused {
    /// Its root plate has no rows: a plate the user cannot leave by choosing
    /// anything is not a menu.
    NoRows,
}

/// Why one of the desktop's own menus did not come up.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DesktopMenuRefused {
    /// The seat cannot show a menu at all: there is no output to place on, or
    /// a surface a menu may not displace holds it.
    Seat(MenuRefusal),
    /// The model itself cannot be shown.
    Model(ModelRefused),
}

/// Open one of the desktop's own menus as the seat's one chain.
///
/// The backdrop's and the icon bar's menus are clients of the menu service
/// exactly as an application's is; the only difference is that their models are
/// built in process, so their rows may state things an application
/// structurally cannot. Both arrive through here, so a desktop press cannot
/// take the grab from the lock screen or the trusted picker by arriving from a
/// direction that skipped the seat rule — the same rule an application's
/// `OpenMenu` resolves through.
///
/// # Errors
///
/// [`DesktopMenuRefused`] when the seat will not show a menu or the model
/// cannot be drawn as one; the chain that was up is left exactly as it was.
pub fn open_desktop_menu(
    chain: &mut MenuChain,
    owner: ChainOwner,
    model: ChainModel,
    placement: PlatePlacement,
    seat_held: bool,
    geom: &ChainGeometry<'_>,
) -> Result<(), DesktopMenuRefused> {
    if let Some(reason) = seat_menu_refusal(geom.screen, seat_held) {
        return Err(DesktopMenuRefused::Seat(reason));
    }
    chain
        .open(owner, model, placement, geom)
        .map_err(DesktopMenuRefused::Model)
}

impl OpenChain {
    /// The chain ending with `outcome`, recorded so `MenuChain::handle` can
    /// queue the answer and snapshot the surfaces the chain still holds.
    fn closing(&mut self, outcome: ChainOutcome) -> ChainAction {
        self.outcome = Some(outcome);
        ChainAction::Closed
    }

    /// Measure every plate and the information panel again, re-place what is
    /// not pinned, and owe every surface whole.
    fn refit(&mut self, geom: &ChainGeometry<'_>) {
        for plate in self.plates.iter_mut().filter(|plate| plate.pinned) {
            let (width, height) = plate_extent(plate, geom);
            plate.rect = Rect::new(plate.rect.left(), plate.rect.top(), width, height);
        }
        if let Some(panel) = self.info.as_mut() {
            let (width, height) = panel.sheet.measured_size(geom.scale, geom.theme);
            panel.rect = Rect::new(
                panel.rect.left(),
                panel.rect.top(),
                width.max(1),
                height.max(1),
            );
        }
        self.replace_geometry(geom);
        for plate in &mut self.plates {
            plate.repaint = Repaint::Whole;
        }
        if let Some(panel) = self.info.as_mut() {
            panel.repaint = Repaint::Whole;
        }
        if let Some(entry) = self.entry.as_mut() {
            entry.repaint = Repaint::Whole;
        }
    }

    /// Re-derive every unpinned plate's placement, and whatever hangs off the
    /// deepest one.
    ///
    /// A dragged plate keeps the position the user gave it; its children are
    /// placed against it as usual, so a chain drags as one piece and its
    /// descendants stay edge-adjacent.
    fn replace_geometry(&mut self, geom: &ChainGeometry<'_>) {
        for depth in 0..self.plates.len() {
            if self.plates[depth].pinned {
                continue;
            }
            let (width, height) = plate_extent(&self.plates[depth], geom);
            let rect = match depth.checked_sub(1) {
                None => plate_rect(width, height, self.placement, geom.screen),
                Some(parent) => {
                    let against = self.child_anchor(parent, geom);
                    plate_rect(
                        width,
                        height,
                        PlatePlacement::adjacent(against),
                        geom.screen,
                    )
                }
            };
            self.plates[depth].rect = rect;
        }
        if let Some((width, height)) = self
            .info
            .as_ref()
            .map(|panel| (panel.rect.width, panel.rect.height))
        {
            let rect = self.hang(width, height, geom);
            if let Some(panel) = self.info.as_mut() {
                panel.rect = rect;
            }
        }
        if let Some((width, height)) = self
            .entry
            .as_ref()
            .map(|entry| (entry.rect.width, entry.rect.height))
        {
            let rect = self.hang(width, height, geom);
            if let Some(entry) = self.entry.as_mut() {
                entry.rect = rect;
            }
        }
    }

    /// The region a child of the plate at `depth` is placed against: that
    /// plate's own horizontal span, lifted from the row whose child it is by
    /// the child's [`body_lead`], so the child's body — a submenu's first row
    /// — lies level with that row and its band stands above it.
    fn child_anchor(&self, depth: usize, geom: &ChainGeometry<'_>) -> Rect {
        let Some(plate) = self.plates.get(depth) else {
            return Rect::EMPTY;
        };
        let row = plate
            .open_row
            .and_then(|index| row_rect(plate, index, geom))
            .unwrap_or(plate.rect);
        Rect::new(
            plate.rect.left(),
            row.top().saturating_sub_unsigned(body_lead(geom)),
            plate.rect.width,
            row.height,
        )
    }

    /// Where a `width` × `height` information panel hangs: exactly where the
    /// child plate of the same row would, so a panel and a submenu are placed
    /// by one rule.
    fn hang(&self, width: u32, height: u32, geom: &ChainGeometry<'_>) -> Rect {
        let deepest = self.plates.len().saturating_sub(1);
        let against = self.child_anchor(deepest, geom);
        plate_rect(
            width,
            height,
            PlatePlacement::adjacent(against),
            geom.screen,
        )
    }

    /// Close every plate deeper than `depth`, and anything hanging off them.
    fn truncate_to(&mut self, depth: usize) {
        self.plates.truncate(depth + 1);
        self.info = None;
        // A field the user never committed answers nothing: the text is the
        // commit, so closing the surface discards it rather than reporting a
        // half-typed name.
        self.entry = None;
        if let Some(plate) = self.plates.get_mut(depth) {
            plate.open_row = None;
        }
    }

    /// The chain's surface the pointer is over, deepest first — a child is
    /// drawn over its parent, so the deepest match owns the point.
    fn surface_at(&self, pointer: Point) -> Option<Hit> {
        if self.info.as_ref().is_some_and(|p| p.rect.contains(pointer)) {
            return Some(Hit::Info);
        }
        if self
            .entry
            .as_ref()
            .is_some_and(|e| e.rect.contains(pointer))
        {
            return Some(Hit::Entry);
        }
        self.plates
            .iter()
            .enumerate()
            .rev()
            .find(|(_, plate)| plate.rect.contains(pointer))
            .map(|(depth, _)| Hit::Plate(depth))
    }

    /// Route a pointer event.
    fn on_pointer(
        &mut self,
        event: &InputEvent,
        pointer: Point,
        geom: &ChainGeometry<'_>,
        now_ns: u64,
    ) -> ChainAction {
        let moved = matches!(event, InputEvent::PointerMoved { .. });
        let from = if moved {
            self.last_pointer.replace(pointer)
        } else {
            None
        };
        if let Some(action) = self.drag_pointer(event, pointer, geom) {
            return action;
        }
        let hit = self.surface_at(pointer);
        // A deferral lives only while the pointer moves over the plate it
        // deferred on: a press, a scroll, or reaching the child ends it.
        if !moved
            || !matches!(hit, Some(Hit::Plate(depth)) if self.settle.is_some_and(|s| s.depth == depth))
        {
            self.settle = None;
        }
        let reclaimed = self.reclaim_highlight(hit, geom);
        let acted = match (event, hit) {
            // A press with nothing of the chain under it ends the chain, and
            // the press goes no further: a dismissal is not also a click on
            // what the menu was covering.
            (InputEvent::PointerPressed { .. }, None) => self.closing(ChainOutcome::Dismissed),
            // The information panel states facts and offers no action, so a
            // pointer over it is claimed and does nothing.
            (_, Some(Hit::Info) | None) => ChainAction::Consumed,
            // The field is a control, so a press in it positions its caret.
            (_, Some(Hit::Entry)) => self.entry_pointer(event, geom),
            (_, Some(Hit::Plate(depth))) => {
                self.plate_pointer(depth, event, (from, pointer), geom, now_ns)
            }
        };
        if reclaimed && acted == ChainAction::Consumed {
            ChainAction::Redraw
        } else {
            acted
        }
    }

    /// Give the open row back the highlight it lent, unless `keep` is the
    /// plate that borrowed it. Answers whether that moved the highlight.
    fn reclaim_highlight(&mut self, keep: Option<Hit>, geom: &ChainGeometry<'_>) -> bool {
        let Some(depth) = self.lent else {
            return false;
        };
        if keep == Some(Hit::Plate(depth)) {
            return false;
        }
        self.lent = None;
        let Some(plate) = self.plates.get_mut(depth) else {
            return false;
        };
        plate.menu.hold(plate.open_row);
        let rows = rows_rect(plate, geom);
        let mut damage = damage::sink();
        plate
            .menu
            .refresh_hover(rows, geom.scale, geom.theme, &mut damage);
        self.mark_plate(depth, &damage);
        !damage.is_empty()
    }

    /// Whether row `index` of the plate at `depth` can open or do anything.
    fn actionable(&self, depth: usize, index: usize) -> bool {
        self.plates
            .get(depth)
            .and_then(|plate| plate.rows.get(index))
            .and_then(|&row| self.model.rows().get(row))
            .is_some_and(|entry| entry.drawn().state().is_actionable())
    }

    /// Route a pointer event that landed on the plate at `depth`, at `pointer`
    /// from the previous sample.
    fn plate_pointer(
        &mut self,
        depth: usize,
        event: &InputEvent,
        (from, pointer): (Option<Point>, Point),
        geom: &ChainGeometry<'_>,
        now_ns: u64,
    ) -> ChainAction {
        let Some(plate) = self.plates.get_mut(depth) else {
            return ChainAction::Consumed;
        };
        let band = Rect::new(
            plate.rect.left(),
            plate.rect.top(),
            plate.rect.width,
            TitleBar::band_height(geom.scale, geom.theme).min(plate.rect.height),
        );
        let mut damage = damage::sink();
        if band.contains(pointer) {
            // The band is no row, so a crossing waiting there has not been
            // rested on, and no row the pointer left keeps a lent highlight.
            self.settle = None;
            let reclaimed = self.reclaim_highlight(None, geom);
            let Some(plate) = self.plates.get_mut(depth) else {
                return ChainAction::Consumed;
            };
            let acted = plate
                .band
                .on_pointer(event, band, geom.scale, geom.theme, &mut damage);
            self.mark_plate(depth, &damage);
            let answered = self.band_event(depth, acted, pointer, geom);
            return if reclaimed && answered == ChainAction::Consumed {
                ChainAction::Redraw
            } else {
                answered
            };
        }
        let rows = rows_rect(plate, geom);
        if matches!(event, InputEvent::PointerMoved { .. }) {
            let over = plate.menu.row_at(rows, geom.scale, geom.theme, pointer);
            let steered = self.steer(depth, over, (from, pointer), now_ns, geom);
            let open = self.plates.get(depth).and_then(|plate| plate.open_row);
            // The row whose child is open keeps the highlight however the
            // pointer reaches the child, except to a row that can open
            // nothing, which borrows it while the pointer rests there.
            let lend = open.is_some()
                && self.settle.is_none()
                && over.is_some_and(|row| Some(row) != open && !self.actionable(depth, row));
            self.lent = lend.then_some(depth);
            let Some(plate) = self.plates.get_mut(depth) else {
                return steered;
            };
            // Whether the highlight moved is the row control's answer, so
            // travel within one row costs no repaint.
            plate.menu.hold(if lend { None } else { plate.open_row });
            let _ = plate
                .menu
                .on_pointer(event, rows, geom.scale, geom.theme, &mut damage);
            self.mark_plate(depth, &damage);
            return if steered == ChainAction::Consumed && !damage.is_empty() {
                ChainAction::Redraw
            } else {
                steered
            };
        }
        let acted = plate
            .menu
            .on_pointer(event, rows, geom.scale, geom.theme, &mut damage);
        self.mark_plate(depth, &damage);
        match acted {
            Some(MenuAction::Activated { index }) => self.choose(depth, index, geom),
            // A click on a row that opens a child keeps its child open
            // rather than acting: the child is what the row is for.
            Some(MenuAction::OpenSubmenu { index }) => self.arrive(depth, index, geom),
            Some(MenuAction::Dismissed) => self.closing(ChainOutcome::Dismissed),
            None if damage.is_empty() => ChainAction::Consumed,
            None => ChainAction::Redraw,
        }
    }

    /// Decide what a pointer step onto `over` of the plate at `depth` means.
    ///
    /// A point on no row changes nothing: the rim and a group break lie
    /// between rows, never on a different one. A step onto a sibling of the row
    /// whose child is open waits while it heads into that child, so a diagonal
    /// path to the child may cross the row beneath; any other step chooses the
    /// row at once. A step that goes nowhere decides nothing.
    fn steer(
        &mut self,
        depth: usize,
        over: Option<usize>,
        (from, to): (Option<Point>, Point),
        now_ns: u64,
        geom: &ChainGeometry<'_>,
    ) -> ChainAction {
        if from == Some(to) {
            return ChainAction::Consumed;
        }
        let Some(row) = over else {
            self.settle = None;
            return ChainAction::Consumed;
        };
        let open = self.plates.get(depth).and_then(|plate| plate.open_row);
        if open == Some(row) {
            self.settle = None;
            self.aim_apex = Some(to);
            return ChainAction::Consumed;
        }
        let slack = geom.scale.scale_length(AIM_SLACK).max(1);
        let aiming = open.is_some()
            && self.aim_apex.is_some_and(|apex| {
                let parent = self.plates.get(depth).map(|plate| plate.rect);
                parent
                    .zip(self.child_rect(depth))
                    .is_some_and(|(parent, child)| heads_into(apex, to, child, parent, slack))
            });
        if aiming {
            self.settle = Some(Settle {
                depth,
                row,
                due_ns: now_ns.saturating_add(SUBMENU_SETTLE_NS),
            });
            return ChainAction::Consumed;
        }
        self.settle = None;
        self.arrive(depth, row, geom)
    }

    /// The surface hanging off the plate at `depth`: the next plate down, or the
    /// panel or field on the deepest.
    fn child_rect(&self, depth: usize) -> Option<Rect> {
        if let Some(plate) = self.plates.get(depth + 1) {
            return Some(plate.rect);
        }
        if depth + 1 != self.plates.len() {
            return None;
        }
        self.info
            .as_ref()
            .map(|info| info.rect)
            .or_else(|| self.entry.as_ref().map(|entry| entry.rect))
    }

    /// Make a deferred crossing that has come due the choice it was waiting to
    /// be: the pointer rested on the crossed row.
    fn settle_due(&mut self, now_ns: u64, geom: &ChainGeometry<'_>) -> bool {
        let Some(settle) = self.settle.filter(|settle| now_ns >= settle.due_ns) else {
            return false;
        };
        self.settle = None;
        let changed = self.arrive(settle.depth, settle.row, geom) != ChainAction::Consumed;
        let Some(plate) = self.plates.get_mut(settle.depth) else {
            return changed;
        };
        plate.menu.hold(plate.open_row);
        let rows = rows_rect(plate, geom);
        let mut damage = damage::sink();
        plate
            .menu
            .refresh_hover(rows, geom.scale, geom.theme, &mut damage);
        self.mark_plate(settle.depth, &damage);
        changed || !damage.is_empty()
    }

    /// Fold a control's reported damage — screen rectangles, because that is
    /// the space a plate's rows are laid out and hit-tested in — into what the
    /// plate at `depth` owes, in that plate's own local pixels.
    fn mark_plate(&mut self, depth: usize, damage: &Region) {
        if damage.is_empty() {
            return;
        }
        let Some(plate) = self.plates.get_mut(depth) else {
            return;
        };
        let origin = plate.rect.origin;
        for rect in damage.rects() {
            plate.repaint.add(Rect::new(
                rect.left().saturating_sub(origin.x),
                rect.top().saturating_sub(origin.y),
                rect.width,
                rect.height,
            ));
        }
    }

    /// Apply whatever the band reported for the plate at `depth`.
    fn band_event(
        &mut self,
        depth: usize,
        acted: Option<TitleBarEvent>,
        pointer: Point,
        geom: &ChainGeometry<'_>,
    ) -> ChainAction {
        match acted {
            // The press itself takes the pointer, before the threshold has
            // decided whether it is a drag: a gesture that crosses off the
            // band must still be the one the press began.
            Some(TitleBarEvent::Activate) => {
                self.drag = Some(Drag {
                    plate: depth,
                    from: pointer,
                });
                ChainAction::Consumed
            }
            Some(TitleBarEvent::DragBegin) => {
                // Dragging pins the plate: its placement stops being derived
                // from the anchor, because the user put it where it is. The
                // motion that crossed the threshold is part of the drag, so it
                // moves the plate too — otherwise a short drag ending on that
                // very motion leaves the plate where it started.
                if let Some(plate) = self.plates.get_mut(depth) {
                    plate.pinned = true;
                }
                self.drag_to(pointer, geom)
            }
            Some(TitleBarEvent::DragMoved { to }) => self.drag_to(to, geom),
            // A plate band seats no commands, so nothing else it can report
            // means anything here.
            Some(
                TitleBarEvent::DragEnd
                | TitleBarEvent::Control(_)
                | TitleBarEvent::AlternateControl(_),
            )
            | None => ChainAction::Consumed,
        }
    }

    /// Feed a band press that has taken the pointer, wherever it has gone.
    fn drag_pointer(
        &mut self,
        event: &InputEvent,
        pointer: Point,
        geom: &ChainGeometry<'_>,
    ) -> Option<ChainAction> {
        let depth = self.drag?.plate;
        let plate = self.plates.get_mut(depth)?;
        let band = Rect::new(
            plate.rect.left(),
            plate.rect.top(),
            plate.rect.width,
            TitleBar::band_height(geom.scale, geom.theme).min(plate.rect.height),
        );
        let mut damage = Region::new();
        let acted = plate
            .band
            .on_pointer(event, band, geom.scale, geom.theme, &mut damage);
        let action = self.band_event(depth, acted, pointer, geom);
        // The button coming up ends the gesture whether or not it ever passed
        // the drag threshold, so a press that merely tapped the band does not
        // leave the pointer held.
        if matches!(event, InputEvent::PointerReleased { .. }) {
            self.drag = None;
        }
        Some(action)
    }

    /// Move the dragged plate and its descendants to follow the pointer.
    ///
    /// Ancestors stay put: a press on a band moves that plate and what hangs
    /// beneath it, which is what lets a user pull a submenu aside to read what
    /// is under it without disturbing the chain it came from.
    fn drag_to(&mut self, to: Point, geom: &ChainGeometry<'_>) -> ChainAction {
        let Some(drag) = self.drag.as_mut() else {
            return ChainAction::Consumed;
        };
        let depth = drag.plate;
        let dx = to.x - drag.from.x;
        let dy = to.y - drag.from.y;
        drag.from = to;
        if dx == 0 && dy == 0 {
            return ChainAction::Consumed;
        }
        let Some(plate) = self.plates.get_mut(depth) else {
            return ChainAction::Consumed;
        };
        plate.rect = Rect::new(
            plate.rect.left() + dx,
            plate.rect.top() + dy,
            plate.rect.width,
            plate.rect.height,
        )
        .clamped_onto(geom.screen);
        // A descendant is placed against its parent, so re-deriving the
        // unpinned ones below is what makes the chain travel as one piece.
        for below in self.plates.iter_mut().skip(depth + 1) {
            below.pinned = false;
        }
        self.replace_geometry(geom);
        ChainAction::Redraw
    }

    /// The pointer settled on row `index` of the plate at `depth`: it opens
    /// that row's child, closing whatever the plate had open.
    fn arrive(&mut self, depth: usize, index: usize, geom: &ChainGeometry<'_>) -> ChainAction {
        let Some(plate) = self.plates.get(depth) else {
            return ChainAction::Consumed;
        };
        if plate.open_row == Some(index) {
            return ChainAction::Consumed;
        }
        let Some(&model_row) = plate.rows.get(index) else {
            return ChainAction::Consumed;
        };
        let Some(entry) = self.model.rows().get(model_row) else {
            return ChainAction::Consumed;
        };
        // A disabled row opens nothing, and leaves whatever was open alone: a
        // pointer resting on a row it cannot use has not chosen to close
        // anything.
        if !entry.drawn().state().is_actionable() {
            return ChainAction::Consumed;
        }
        let child = entry.child().clone();
        // A field's band is titled from the row it hangs off, taken before
        // the model borrow ends — and only for the child that wants one, so
        // an ordinary arrival allocates nothing.
        let entry_title =
            matches!(child, ChainChild::Entry { .. }).then(|| entry.drawn().label().to_string());
        // Whether anything was actually taken down, which is what tells a
        // no-child arrival from one that closed a plate.
        let closed = self.plates.len() > depth + 1 || self.info.is_some() || self.entry.is_some();
        self.truncate_to(depth);
        if let Some(plate) = self.plates.get_mut(depth) {
            plate.open_row = Some(index);
        }
        self.aim_apex = self.last_pointer;
        match child {
            // A row with no child opened nothing, so whether anything has to
            // be redrawn is the highlight's question, not this one's.
            ChainChild::None => {
                if let Some(plate) = self.plates.get_mut(depth) {
                    plate.open_row = None;
                }
                if closed {
                    ChainAction::Redraw
                } else {
                    ChainAction::Consumed
                }
            }
            ChainChild::Submenu => {
                match plate_for(&self.model, Some(model_row)) {
                    Ok(child) => self.plates.push(child),
                    // A submenu row whose plate holds nothing opens nothing,
                    // rather than a plate with no way out of it.
                    Err(ModelRefused::NoRows) => {
                        if let Some(plate) = self.plates.get_mut(depth) {
                            plate.open_row = None;
                        }
                        return ChainAction::Redraw;
                    }
                }
                self.replace_geometry(geom);
                ChainAction::Redraw
            }
            ChainChild::Info(sheet) => {
                let (width, height) = sheet.measured_size(geom.scale, geom.theme);
                self.info = Some(InfoPanel {
                    sheet,
                    rect: self.hang(width.max(1), height.max(1), geom),
                    repaint: Repaint::Whole,
                });
                ChainAction::Redraw
            }
            ChainChild::Entry {
                id,
                initial,
                selected,
            } => {
                let (width, height) = entry_extent(geom);
                let mut band = TitleBar::plate();
                band.set_title(&entry_title.unwrap_or_default());
                let mut field = TextField::new()
                    .with_text(&initial)
                    .with_max_len(APP_MENU_ENTRY_MAX)
                    .with_selection(selected);
                field.set_focused(true);
                self.entry = Some(EntrySurface {
                    id,
                    band,
                    field,
                    rect: self.hang(width, height, geom),
                    repaint: Repaint::Whole,
                });
                ChainAction::Redraw
            }
        }
    }

    /// Route a pointer event that landed on the quick-entry surface.
    ///
    /// Everything goes to the field, so a press inside it positions its caret
    /// rather than dismissing the chain. The band above it titles the surface
    /// and does not drag it: where a plate is placed by the user, a child of a
    /// row is placed by the chain against the row it hangs off — the same rule
    /// the information panel is placed by — so there is nothing for a press on
    /// it to move.
    fn entry_pointer(&mut self, event: &InputEvent, geom: &ChainGeometry<'_>) -> ChainAction {
        let Some(entry) = self.entry.as_mut() else {
            return ChainAction::Consumed;
        };
        let field = entry.field_rect(geom);
        let mut damage = damage::sink();
        let acted = entry
            .field
            .on_pointer(event, field, geom.scale, geom.theme, &mut damage);
        let owed = entry.owe(&damage);
        match acted {
            Some(action) => self.entry_action(action),
            None => owed,
        }
    }

    /// Apply what the quick-entry field reported.
    ///
    /// A commit is the chain's answer: the text *is* the outcome, so it ends
    /// the chain rather than leaving a surface up that has already said
    /// everything it had to say. A cancel closes the field alone, so Escape
    /// gets out of it before it gets out of the chain.
    fn entry_action(&mut self, action: TextAction) -> ChainAction {
        let Some(entry) = self.entry.as_ref() else {
            return ChainAction::Consumed;
        };
        match action {
            TextAction::Submitted => {
                let answer = ChainOutcome::Entered(entry.id, entry.field.text().to_string());
                self.closing(answer)
            }
            TextAction::Cancelled => {
                self.entry = None;
                ChainAction::Redraw
            }
            TextAction::Edited => ChainAction::Redraw,
        }
    }

    /// A row of the plate at `depth` was activated.
    ///
    /// A row carrying an id answers the chain with it; one that carries none
    /// is a row whose whole purpose is its child, so activating it opens or
    /// keeps that child. That split is the model's to make, which is why the
    /// shared control reports every activation and never guesses what a
    /// chevron implies.
    fn choose(&mut self, depth: usize, index: usize, geom: &ChainGeometry<'_>) -> ChainAction {
        let Some(&model_row) = self.plates.get(depth).and_then(|p| p.rows.get(index)) else {
            return ChainAction::Consumed;
        };
        let Some(entry) = self.model.rows().get(model_row) else {
            return ChainAction::Consumed;
        };
        match entry.id() {
            Some(id) => self.closing(ChainOutcome::Chosen(id)),
            // A submenu and the information row name no id, so there is
            // nothing for choosing one to answer: activating one opens or
            // keeps the child it is for.
            None => self.arrive(depth, index, geom),
        }
    }

    /// Route a key. Traversal is the service's, not any application's.
    fn on_key(&mut self, key: Key, modifiers: Modifiers, geom: &ChainGeometry<'_>) -> ChainAction {
        let deepest = self.plates.len().saturating_sub(1);
        // An open field owns the keyboard: a menu's traversal keys are text
        // in a field, and the field's own Escape closes it before the chain's
        // would dismiss the whole chain.
        if self.entry.is_some() {
            return self.entry_key(key, modifiers, geom);
        }
        // Escape closes the deepest open child first, so repeated Escape
        // always gets the user out.
        if key == Key::Named(NamedKey::Escape) {
            if self.info.is_some() {
                self.truncate_to(deepest);
                return ChainAction::Redraw;
            }
            if deepest == 0 {
                return self.closing(ChainOutcome::Dismissed);
            }
            self.truncate_to(deepest - 1);
            return ChainAction::Redraw;
        }
        if key == Key::Named(NamedKey::Left) {
            if deepest == 0 && self.info.is_none() {
                return ChainAction::Consumed;
            }
            let back = if self.info.is_some() {
                deepest
            } else {
                deepest - 1
            };
            self.truncate_to(back);
            return ChainAction::Redraw;
        }
        let Some(plate) = self.plates.get_mut(deepest) else {
            return ChainAction::Consumed;
        };
        let rows = rows_rect(plate, geom);
        let mut damage = damage::sink();
        let acted = plate
            .menu
            .on_key(key, rows, geom.scale, geom.theme, &mut damage);
        self.mark_plate(deepest, &damage);
        match acted {
            Some(MenuAction::Activated { index }) => self.choose(deepest, index, geom),
            Some(MenuAction::OpenSubmenu { index }) => self.arrive(deepest, index, geom),
            Some(MenuAction::Dismissed) => self.closing(ChainOutcome::Dismissed),
            None => {
                if damage.is_empty() {
                    ChainAction::Consumed
                } else {
                    ChainAction::Redraw
                }
            }
        }
    }
}

impl OpenChain {
    /// Route a key into the open quick-entry field.
    ///
    /// Everything reaches the field, so a name containing `h` is typed rather
    /// than moving a highlight. The field's own commit and cancel are the two
    /// that mean anything to the chain.
    fn entry_key(
        &mut self,
        key: Key,
        modifiers: Modifiers,
        geom: &ChainGeometry<'_>,
    ) -> ChainAction {
        let Some(entry) = self.entry.as_mut() else {
            return ChainAction::Consumed;
        };
        let field = entry.field_rect(geom);
        let mut damage = damage::sink();
        let acted = entry.field.on_key(key, modifiers, field, &mut damage);
        let owed = entry.owe(&damage);
        match acted {
            Some(action) => self.entry_action(action),
            None => owed,
        }
    }
}

/// The answer a chain that has ended settled on, defaulting closed: an ended
/// chain always owes exactly one answer, and a dismissal is the answer that
/// grants nothing.
fn chain_outcome(chain: Option<&mut OpenChain>) -> ChainOutcome {
    chain
        .and_then(|chain| chain.outcome.take())
        .unwrap_or(ChainOutcome::Dismissed)
}

/// Which surface of the chain a point landed on.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Hit {
    /// The plate at this depth.
    Plate(usize),
    /// The information panel hanging off the deepest plate.
    Info,
    /// The quick-entry surface hanging off the deepest plate.
    Entry,
}

/// Build the plate holding the rows filed under `opener` (`None` for the
/// root).
///
/// # Errors
///
/// [`ModelRefused::NoRows`] when that plate would hold nothing.
fn plate_for(model: &ChainModel, opener: Option<usize>) -> Result<Plate, ModelRefused> {
    let rows: Vec<usize> = model
        .rows()
        .iter()
        .enumerate()
        .filter(|(_, row)| row.parent() == opener)
        .map(|(index, _)| index)
        .collect();
    if rows.is_empty() {
        return Err(ModelRefused::NoRows);
    }
    let items: Vec<tairix_controls::MenuItem> = rows
        .iter()
        .filter_map(|&index| model.rows().get(index))
        .map(|row| row.drawn().clone())
        .collect();
    let mut band = TitleBar::plate();
    // A submenu's band states its parent row's label; the root's states the
    // title the chain was opened with. Neither is a new wire field, and both
    // go through the same untrusted-label bounding a window title does.
    band.set_title(&match opener {
        None => model.title().to_string(),
        Some(at) => model
            .rows()
            .get(at)
            .map_or_else(String::new, |row| row.drawn().label().to_string()),
    });
    Ok(Plate {
        rows,
        menu: Menu::new(items),
        band,
        rect: Rect::EMPTY,
        pinned: false,
        open_row: None,
        repaint: Repaint::Whole,
    })
}

/// Lay the shared floating-plate ground over a `size` surface, its top `band`
/// rows in the ground of the heading band they carry: the recipe every chain
/// surface stands on, plate and information panel alike.
///
/// The rectangle it lands on has been cleared by
/// [`tairix_controls::damage::paint_parts`], which is what
/// lets a repaint of part of a retained plate land the same pixels: a
/// translucent plate's arc pixels are blended by coverage, so laying the ground
/// over what one already held would tint the corner.
fn lay_plate(
    surface: &mut tairix_raster::Surface,
    size: (u32, u32),
    band: u32,
    geom: &ChainGeometry<'_>,
) {
    let (width, height) = size;
    let _ = tairix_controls::paint_titled_surface_plate(
        surface,
        (0, 0, width, height),
        (
            geom.scale
                .scale_length(geom.theme.metrics().popup_corner_radius),
            tairix_controls::plate_border(geom.theme, geom.scale),
        ),
        band,
        geom.theme,
        (
            geom.theme.palette().surface_raised,
            tairix_controls::ChromeLayer::Ground,
        ),
    );
}

/// The quick-entry surface's extent: its band over one line of field.
fn entry_extent(geom: &ChainGeometry<'_>) -> (u32, u32) {
    let pad = geom
        .scale
        .scale_length(geom.theme.metrics().control_inset)
        .max(1);
    let width = geom.scale.scale_length(ENTRY_SURFACE_WIDTH).max(1);
    let height = TitleBar::band_height(geom.scale, geom.theme)
        .saturating_add(TextField::height(geom.scale, geom.theme))
        .saturating_add(pad.saturating_mul(2))
        .max(1);
    (width, height)
}

/// Where the field sits inside an entry surface whose own rectangle is
/// `surface` and whose band is `band_h` tall: inset from the plate's edges,
/// below the band.
///
/// It insets from `surface`'s own origin, so the answer is in whatever space
/// `surface` was given in — the paint asks in the surface's local pixels and
/// the input routes ask in screen pixels, off one definition, so a caret
/// cannot land where the glyphs are not.
fn entry_field_rect(surface: Rect, band_h: u32, geom: &ChainGeometry<'_>) -> Rect {
    let pad = geom
        .scale
        .scale_length(geom.theme.metrics().control_inset)
        .max(1);
    Rect::new(
        surface.left().saturating_add_unsigned(pad),
        surface
            .top()
            .saturating_add_unsigned(band_h.saturating_add(pad)),
        surface.width.saturating_sub(pad.saturating_mul(2)),
        surface
            .height
            .saturating_sub(band_h.saturating_add(pad.saturating_mul(2))),
    )
}

/// A plate's preferred extent: the band over the rows, never narrower than a
/// band can be drawn or than its own title needs.
///
/// A plate is chrome the desktop sizes to its content, so a title it has the
/// freedom to show is never elided to fit rows that happen to be shorter.
fn plate_extent(plate: &Plate, geom: &ChainGeometry<'_>) -> (u32, u32) {
    let band_h = TitleBar::band_height(geom.scale, geom.theme);
    let width = plate
        .menu
        .preferred_width(geom.scale, geom.theme)
        .max(plate.band.preferred_band_width(geom.scale, geom.theme))
        .max(1);
    let height = band_h
        .saturating_add(plate.menu.preferred_height(geom.scale, geom.theme))
        .max(1);
    (width, height)
}

/// Whether a path from `apex` that has reached `to` heads into `child`, which
/// hangs beside `parent`: `to` is still within `slack` of the apex, where a
/// path has no heading yet, or lies in the triangle from the apex to the
/// child's near edge, the band a pointer aimed anywhere at the child moves
/// through.
fn heads_into(apex: Point, to: Point, child: Rect, parent: Rect, slack: u32) -> bool {
    if to.x.abs_diff(apex.x) <= slack && to.y.abs_diff(apex.y) <= slack {
        return true;
    }
    let doubled_centre = |rect: Rect| i64::from(rect.left()) * 2 + i64::from(rect.width);
    let near = if doubled_centre(child) >= doubled_centre(parent) {
        child.left()
    } else {
        child.right()
    };
    in_triangle(
        to,
        [
            apex,
            Point::new(near, child.top()),
            Point::new(near, child.bottom()),
        ],
    )
}

/// Whether `p` lies inside or on the triangle `corners`: on no side of any
/// edge opposite to the others.
fn in_triangle(p: Point, [a, b, c]: [Point; 3]) -> bool {
    let side = |u: Point, v: Point| {
        (i64::from(v.x) - i64::from(u.x)) * (i64::from(p.y) - i64::from(u.y))
            - (i64::from(v.y) - i64::from(u.y)) * (i64::from(p.x) - i64::from(u.x))
    };
    let turns = [side(a, b), side(b, c), side(c, a)];
    !(turns.iter().any(|&t| t < 0) && turns.iter().any(|&t| t > 0))
}

/// How far below a chain surface's top its body begins: the band, then the
/// plate's rim, which is where a plate's first row starts.
fn body_lead(geom: &ChainGeometry<'_>) -> u32 {
    TitleBar::band_height(geom.scale, geom.theme)
        .saturating_add(tairix_controls::plate_border(geom.theme, geom.scale))
}

/// The screen rectangle a plate's rows occupy, below its band.
fn rows_rect(plate: &Plate, geom: &ChainGeometry<'_>) -> Rect {
    let band_h = TitleBar::band_height(geom.scale, geom.theme).min(plate.rect.height);
    Rect::new(
        plate.rect.left(),
        plate.rect.top().saturating_add_unsigned(band_h),
        plate.rect.width,
        plate.rect.height.saturating_sub(band_h),
    )
}

/// The screen rectangle of row `index` of `plate`.
fn row_rect(plate: &Plate, index: usize, geom: &ChainGeometry<'_>) -> Option<Rect> {
    plate
        .menu
        .row_rect(index, rows_rect(plate, geom), geom.scale, geom.theme)
}

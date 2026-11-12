//! The client half: the app-side presenter and event wait.
//!
//! [`WindowClient`] speaks the wire protocol over an injected
//! [`WindowTransport`] (the `ipc_call` syscall in production, a mock in
//! tests). [`WindowEvents`] wraps an injected [`EventSource`] — the
//! app's **parked** wait on its own event endpoint, never a poll — and
//! decodes each delivered event fail-closed.
//!
//! # Zero-copy shape
//!
//! The app owns the shared frame region (its own `shm_create` mapping):
//! it renders into a frame, then presents by frame *index* plus the
//! damage rectangle it just changed — never pixel bytes. The session
//! reads the pixels through its own mapping of the granted region.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use tairix_display::winframe::damage_in;

use tairix_abi::desktop::DesktopInfo;
use tairix_abi::driver::display::{DamageList, DamageRect, DisplayMode};
use tairix_abi::fs::FileId;
use tairix_abi::reply::decode_status_reply;
use tairix_abi::reply::STATUS_REPLY_LEN;
use tairix_abi::window_ipc::{
    decode_authorised_output_reply, decode_clipboard_reply, decode_connect_reply,
    decode_cursor_sets_reply, decode_drag_spot_reply, decode_drop_target_reply,
    decode_hand_over_reply, decode_handle_reply, decode_menu_text_reply,
    decode_notify_sources_reply, decode_offered_file_reply, decode_offered_items_reply,
    decode_open_target_reply, decode_picked_file_reply, decode_picked_name_reply,
    decode_screensaver_reply, decode_terrain_reply, decode_wallpapers_reply, AlertSeverity,
    AlertSpec, AppBar, AppMenu, AppMenuItemId, BundleRunPath, ClipboardKind, CursorShape,
    DocumentName, DragItems, DropOperation, HandOverOutcome, HandOverTarget, KeyboardMode,
    LandingStep, LayerDepth, NameList, ObjectIds, OpenTarget, PickPurpose, PointerAction,
    PreviewSubject, ScreensaverEntry, SystemTask, TerrainPlate, TooltipText, WallpaperPage,
    WindowEvent, WindowRegion, WindowRequest, WindowTitle, CLIPBOARD_MAX_BYTES,
    DESKTOP_LAYER_MAX_PLATES, DROP_LANDING_MAX_WIRE_LEN, WINDOW_AUTHORISED_OUTPUT_REPLY_MAX,
    WINDOW_CLIPBOARD_REPLY_LEN, WINDOW_CONNECT_REPLY_LEN, WINDOW_CURSOR_SETS_REPLY_MAX,
    WINDOW_DRAG_SPOT_REPLY_MAX, WINDOW_DROP_TARGET_REPLY_MAX, WINDOW_HANDLE_REPLY_LEN,
    WINDOW_HAND_OVER_REPLY_LEN, WINDOW_MENU_TEXT_REPLY_MAX, WINDOW_NOTIFY_SOURCES_REPLY_MAX,
    WINDOW_OFFERED_FILE_REPLY_MAX, WINDOW_OFFERED_ITEMS_REPLY_MAX, WINDOW_OPEN_TARGET_REPLY_MAX,
    WINDOW_PICKED_FILE_REPLY_MAX, WINDOW_PICKED_NAME_REPLY_MAX, WINDOW_SCREENSAVER_REPLY_MAX,
    WINDOW_TERRAIN_REPLY_MAX, WINDOW_WALLPAPERS_REPLY_MAX,
};
use tairix_abi::{Errno, ProcId};
use tairix_geometry::{Point, Rect, Region};
use tairix_input::wire::pointer_button_from_abi;
use tairix_input::{InputEvent, PinchPhase};

use crate::clipboard::{self, Landing};
use crate::server::{DialogSpec, LayerSpec, PopupSpec, ToolSpec, WindowSizeState, WindowSizing};

/// An open target an application pulled, owned rather than borrowed from the
/// reply buffer so the pull can be drained in a loop.
///
/// [`tairix_abi::window_ipc::OpenTarget`]'s owned twin: the wire type
/// borrows from the frame it decoded, which a caller draining a queue cannot
/// hold across the next call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Target {
    /// A path the user named. Confers no access.
    Path(String),
    /// A disk image or archive the user opened, to be attached and shown.
    /// Confers no access.
    Image(String),
    /// A document already opened for this application, reachable through a
    /// one-shot `fd_redeem` handle.
    Document {
        /// Its own file name, for a title. Empty when unknown.
        name: String,
        /// The `fd_redeem` handle. Never zero.
        grant: u64,
        /// Whether the descriptor it redeems is open read-write.
        writable: bool,
    },
    /// A place inside this application, to be resolved against its own
    /// closed set of places. Confers nothing, and one this application does
    /// not recognise leaves it showing what it already showed.
    Pane(String),
    /// A file dropped on this application's window `window_id` at `at`, a
    /// window-local point, to be placed there: reachable read-only through a
    /// one-shot `fd_redeem` handle.
    Placed {
        /// The window it was dropped on.
        window_id: u64,
        /// Where on it, window-local.
        at: (u32, u32),
        /// Its own file name, for display. Empty when unknown.
        name: String,
        /// The `fd_redeem` handle. Never zero.
        grant: u64,
    },
}

const fn widest(lens: &[usize]) -> usize {
    let mut widest = 0;
    let mut at = 0;
    while at < lens.len() {
        if lens[at] > widest {
            widest = lens[at];
        }
        at += 1;
    }
    widest
}

/// Widest reply the client reads into its own buffer: every posted
/// request's, and every call's but those answered into the caller's own page
/// buffer.
///
/// Derived from the requests the channel has rather than from whichever is
/// widest today, so a reply that outgrew the buffer could not slip past.
const REPLY_MAX: usize = widest(&[
    STATUS_REPLY_LEN,
    WINDOW_CONNECT_REPLY_LEN,
    WINDOW_HANDLE_REPLY_LEN,
    WINDOW_HAND_OVER_REPLY_LEN,
    WINDOW_OPEN_TARGET_REPLY_MAX,
    WINDOW_MENU_TEXT_REPLY_MAX,
    WINDOW_CLIPBOARD_REPLY_LEN,
    WINDOW_PICKED_NAME_REPLY_MAX,
    WINDOW_PICKED_FILE_REPLY_MAX,
    WINDOW_DROP_TARGET_REPLY_MAX,
    WINDOW_DRAG_SPOT_REPLY_MAX,
    WINDOW_OFFERED_ITEMS_REPLY_MAX,
    WINDOW_OFFERED_FILE_REPLY_MAX,
    WINDOW_TERRAIN_REPLY_MAX,
    WINDOW_AUTHORISED_OUTPUT_REPLY_MAX,
]);

/// Why a round of input repaints, which is what decides the rectangle it
/// presents (see [`present_damage`]).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Repaint {
    /// Nothing changed, so nothing is presented.
    Nothing,
    /// Controls and their host reported what they changed.
    Reported,
    /// Every pixel may have changed — a first frame, an adopted desktop change,
    /// a resize — so no report could describe it.
    Whole,
}

impl Repaint {
    /// A round that reported its own rectangles when `changed`, else one that
    /// changed nothing.
    #[must_use]
    pub const fn reported_if(changed: bool) -> Self {
        if changed {
            Self::Reported
        } else {
            Self::Nothing
        }
    }

    /// The stronger of two conclusions about one round, so a round that both
    /// reported a rectangle and moved something no report describes still
    /// covers the window.
    #[must_use]
    pub const fn merged(self, other: Self) -> Self {
        match (self, other) {
            (Self::Whole, _) | (_, Self::Whole) => Self::Whole,
            (Self::Reported, _) | (_, Self::Reported) => Self::Reported,
            (Self::Nothing, Self::Nothing) => Self::Nothing,
        }
    }
}

/// The rectangle a round presents, or `None` when it presents nothing.
///
/// A [`Repaint::Reported`] round presents what its controls and host reported,
/// clipped to the window — the whole point of reporting. A round that reported
/// *nothing* presents the whole window instead of nothing at all: over-covering
/// costs pixels, while under-covering would leave a stale frame on screen,
/// because the session copies only what a present declares.
///
/// One definition, because every app that presents what it changed faces the
/// same three cases.
///
/// Presenting less than the whole window is sound because the frame a present
/// is drawn into is first brought up to date with what the window's other
/// frame was presented with since (`WindowPane::present_list`), so it holds the
/// rest of the window's current pixels.
///
/// This is the box spanning everything reported, for an app that repaints one
/// box; an app that paints each reported rectangle alone presents them as
/// they are ([`present_damage_list`]).
#[must_use]
pub fn present_damage(mode: &DisplayMode, repaint: Repaint, damage: &Region) -> Option<DamageRect> {
    match repaint {
        Repaint::Nothing => None,
        Repaint::Whole => Some(DamageRect::full(mode)),
        Repaint::Reported => {
            Some(damage_in(mode, damage.bounds()).unwrap_or_else(|| DamageRect::full(mode)))
        }
    }
}

/// The rectangles a round presents, each clipped to the window, or `None` when
/// it presents nothing: [`present_damage`]'s three cases, for an app that
/// paints each reported rectangle under its own clip. More rectangles than a
/// present carries are merged the least they can be ([`DamageList::fitted`]),
/// so a band's moving edges stay its edges rather than becoming its area.
#[must_use]
pub fn present_damage_list(
    mode: &DisplayMode,
    repaint: Repaint,
    damage: &Region,
) -> Option<DamageList> {
    let mut owed = Owed::new();
    owed.owe(mode, repaint, damage);
    owed.take(mode)
}

/// What a window owes the screen across the rounds since it last presented:
/// the strongest conclusion they reached, and the rectangles they reported.
///
/// A loop that drains its input before painting folds each round in here and
/// presents once, so a burst of pointer samples costs one frame rather than
/// one each. Rectangles are clipped to the window as they are folded and
/// merged by least growth ([`DamageList::fitted`]), so the account stays
/// bounded however long the burst, and a band's moving edges stay its edges
/// rather than becoming its area.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Owed {
    repaint: Repaint,
    parts: Option<DamageList>,
}

impl Default for Owed {
    fn default() -> Self {
        Self::new()
    }
}

impl Owed {
    /// Nothing owed.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            repaint: Repaint::Nothing,
            parts: None,
        }
    }

    /// Whether nothing is owed.
    #[must_use]
    pub const fn is_clean(&self) -> bool {
        matches!(self.repaint, Repaint::Nothing)
    }

    /// Whether every pixel is owed.
    #[must_use]
    pub const fn is_whole(&self) -> bool {
        matches!(self.repaint, Repaint::Whole)
    }

    /// Owe every pixel.
    pub fn owe_whole(&mut self) {
        *self = Self {
            repaint: Repaint::Whole,
            parts: None,
        };
    }

    /// Fold in one round over a window shaped as `mode`: its conclusion, and
    /// the region it reported.
    ///
    /// A reported round naming nothing inside the window still moved pixels it
    /// could not place, so it owes the whole window: over-covering costs
    /// pixels, while under-covering would leave a stale frame on screen,
    /// because the session copies only what a present declares.
    pub fn owe(&mut self, mode: &DisplayMode, repaint: Repaint, damage: &Region) {
        match (self.repaint, repaint) {
            (_, Repaint::Nothing) | (Repaint::Whole, _) => {}
            (_, Repaint::Whole) => self.owe_whole(),
            (_, Repaint::Reported) => {
                let mut reported = damage
                    .rects()
                    .iter()
                    .filter_map(|rect| damage_in(mode, *rect))
                    .peekable();
                if reported.peek().is_none() {
                    self.owe_whole();
                    return;
                }
                let held = self.parts.take();
                self.parts = DamageList::fitted(
                    held.iter()
                        .flat_map(DamageList::rects)
                        .copied()
                        .chain(reported),
                );
                self.repaint = Repaint::Reported;
            }
        }
    }

    /// The rectangles to paint and present for a window shaped as `mode`,
    /// leaving nothing owed, or `None` when nothing was.
    pub fn take(&mut self, mode: &DisplayMode) -> Option<DamageList> {
        let Self { repaint, parts } = core::mem::take(self);
        match repaint {
            Repaint::Nothing => None,
            Repaint::Reported => parts,
            Repaint::Whole => DamageList::fitted([DamageRect::full(mode)]),
        }
    }
}

/// The tooltip a window last asked the session for, so that pointer samples
/// over one tool ask once rather than once each.
///
/// A tip is incidental: a session that shows none refuses, and a refused tip
/// is not asked for again until the one wanted changes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DeclaredTip {
    declared: Option<(WindowRegion, TooltipText)>,
}

impl DeclaredTip {
    /// No tip asked for yet.
    #[must_use]
    pub const fn new() -> Self {
        Self { declared: None }
    }

    /// Ask for `wanted` — the region a tip covers and its line — over window
    /// `window_id`, or for no tip at all, unless it is what was last asked.
    ///
    /// A line or region the wire cannot carry asks for no tip, so the one
    /// last declared is withdrawn rather than left explaining something else.
    pub fn declare<T: WindowTransport>(
        &mut self,
        client: &mut WindowClient<T>,
        window_id: u64,
        wanted: Option<(Rect, &str)>,
    ) {
        let wanted = wanted.and_then(|(rect, text)| {
            let region =
                WindowRegion::new(rect.left(), rect.top(), rect.width, rect.height).ok()?;
            Some((region, TooltipText::new(text).ok()?))
        });
        if wanted == self.declared {
            return;
        }
        self.declared = wanted;
        let sent = match wanted {
            Some(pair) => Ok(pair),
            None => {
                WindowRegion::new(0, 0, 0, 0).and_then(|region| Ok((region, TooltipText::new("")?)))
            }
        };
        if let Ok((region, text)) = sent {
            let _ = client.set_tooltip(window_id, region, text);
        }
    }
}

/// The rectangle a retained window's present must repaint and send, or `None`
/// when nothing of it lies inside the window.
///
/// The whole window when the session has `released` its copy, which holds
/// none of the pixels a partial present would leave standing; otherwise
/// `damage` grown over whatever an earlier present left `torn`. Either is
/// clipped to the window, so a rectangle named past the surface is sent as
/// the part inside it: recorded as torn when its present fails, it can never
/// widen every later present into a rectangle the frame codec refuses.
#[must_use]
pub fn retained_damage(
    mode: &DisplayMode,
    released: bool,
    torn: Option<DamageRect>,
    damage: DamageRect,
) -> Option<DamageRect> {
    if released {
        return Some(DamageRect::full(mode));
    }
    let wanted = torn.map_or(damage, |torn| damage.union(torn));
    let right = wanted.x.saturating_add(wanted.width_px).min(mode.width_px);
    let bottom = wanted
        .y
        .saturating_add(wanted.height_px)
        .min(mode.height_px);
    (wanted.x < right && wanted.y < bottom).then(|| DamageRect {
        x: wanted.x,
        y: wanted.y,
        width_px: right - wanted.x,
        height_px: bottom - wanted.y,
    })
}

/// The rectangles a present into a window shaped as `mode` sends: `damage`, or
/// the whole window when the session has `released` its copy, since the fresh
/// region it lands in holds none of the pixels a partial present would leave
/// standing.
///
/// # Errors
///
/// [`DamageList::new`]'s, for a mode with no pixels.
#[cfg(any(test, all(freestanding, feature = "rt")))]
pub(crate) fn presented_damage(
    mode: &DisplayMode,
    released: bool,
    damage: DamageList,
) -> Result<DamageList, Errno> {
    if released {
        DamageList::new(&[DamageRect::full(mode)])
    } else {
        Ok(damage)
    }
}

/// The alert of `severity` stating what `headline` names and, as its detail,
/// `reason`: a clause written to follow a colon on `stderr`.
///
/// # Errors
///
/// [`AlertSpec::acknowledged`]'s.
pub(crate) fn notice_alert(
    severity: AlertSeverity,
    headline: &str,
    reason: &str,
) -> Result<AlertSpec, Errno> {
    AlertSpec::acknowledged(severity, headline, &alert_sentence(reason))
}

/// `reason` as the sentence an alert states it in: its first letter a capital
/// and a full stop at its end. No reason stays none.
#[must_use]
pub fn alert_sentence(reason: &str) -> String {
    let mut chars = reason.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut sentence = String::with_capacity(reason.len() + 1);
    sentence.extend(first.to_uppercase());
    sentence.push_str(chars.as_str());
    if !sentence.ends_with(['.', '!', '?', '\u{2026}']) {
        sentence.push('.');
    }
    sentence
}

/// The window-local [`Point`] a wire pointer event's `(x, y)` names.
///
/// The protocol carries a pointer position as unsigned window-local
/// coordinates while the controls work in signed screen geometry, so every app
/// that routes a pointer event needs the same saturating widening — one
/// definition here, beside the [`pointer_input_events`] translation it always
/// precedes, rather than a copy in each app's `Run` binary. A coordinate past
/// [`i32::MAX`] saturates there, which is outside every control the app can
/// have laid out, so it hits nothing rather than wrapping onto one.
#[must_use]
pub fn pointer_point(x: u32, y: u32) -> Point {
    Point::new(
        i32::try_from(x).unwrap_or(i32::MAX),
        i32::try_from(y).unwrap_or(i32::MAX),
    )
}

/// The shared input events one delivered [`PointerAction`] at window-local
/// `point` means, in the order a control must receive them.
///
/// Every app that hands pointer input to the shared control family needs the
/// same translation, so it has one definition here rather than a private copy
/// per app. A wire pointer event always carries a position, while the
/// controls' button transitions do not: the position is therefore delivered
/// first as an [`InputEvent::PointerMoved`], and a press or release follows
/// it, so a control is never asked to decide about a button at a position it
/// has not been told about yet. A cancel is the one exception: it ends the
/// press where it stands, so it moves nothing.
///
/// The mapping is total — every action and every button code has exactly one
/// meaning — so an app filters what it does not want by matching the events,
/// never by guessing at an unhandled code.
pub fn pointer_input_events(
    action: PointerAction,
    point: Point,
) -> impl Iterator<Item = InputEvent> {
    let (moved, transition) = match action {
        PointerAction::Moved => (true, None),
        PointerAction::Pressed(button) => (
            true,
            Some(InputEvent::PointerPressed {
                button: pointer_button_from_abi(button),
            }),
        ),
        PointerAction::Released(button) => (
            true,
            Some(InputEvent::PointerReleased {
                button: pointer_button_from_abi(button),
            }),
        ),
        // No move first: a drag the desktop took stays where it was left.
        PointerAction::Cancelled => (false, Some(InputEvent::PointerCancelled)),
    };
    [
        moved.then_some(InputEvent::PointerMoved { to: point }),
        transition,
    ]
    .into_iter()
    .flatten()
}

/// The shared input events one delivered wheel turn at window-local `point`
/// means, in the order a control must receive them: the position first, as
/// for [`pointer_input_events`], so a control is never asked to take a turn at
/// a place it has not been told about, then the turn itself.
pub fn scroll_input_events(point: Point, dx: i32, dy: i32) -> impl Iterator<Item = InputEvent> {
    [
        InputEvent::PointerMoved { to: point },
        InputEvent::PointerScrolled { dx, dy },
    ]
    .into_iter()
}

/// The shared input events one delivered step of a pinch at window-local
/// `point` means: the position first, as for [`scroll_input_events`], then
/// the pinch there.
pub fn pinch_input_events(
    point: Point,
    phase: PinchPhase,
    scale: u32,
) -> impl Iterator<Item = InputEvent> {
    [
        InputEvent::PointerMoved { to: point },
        InputEvent::Pinch {
            phase,
            scale,
            at: point,
        },
    ]
    .into_iter()
}

/// Bounded slot count every window-channel app binds its event mailbox
/// with: input-rate events, drained after every wake, so a small queue is
/// ample and a stalled app costs the kernel a bounded mailbox rather than
/// unbounded memory.
///
/// The depth is one value for the whole channel, not each app's own
/// choice: the session's delivery path treats a refused send as evidence
/// that the owner has stopped draining, so two apps disagreeing about how
/// much slack they grant it would make that evidence mean different things
/// per app. Sized against a drained-every-wake consumer, with the session
/// coalescing an adjacent run of pointer motion over one window into its
/// newest sample before it ever reaches this queue.
///
/// A full mailbox costs the app nothing it cannot recover: the session
/// *holds* the refused event and delivers it once the app drains, parked on
/// the room the drain frees rather than polling for it. So this depth
/// governs how much the kernel buffers, not what an app may miss.
pub const EVENT_MAILBOX_CAPACITY: usize = 32;

/// The calls the client makes of the window endpoint — `ipc_call` and its
/// posted halves behind a seam, so the client is host-testable.
///
/// The endpoint serves one client's requests in the order they reach it,
/// posted or called, so a posted request is served before any call made after
/// it.
pub trait WindowTransport {
    /// Issue one call and wait for its reply, returning the reply length
    /// written into `reply`.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the transport surfaces (no such endpoint, a dead
    /// session); the caller treats a transport failure exactly like a
    /// refused request.
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno>;

    /// Post one request without waiting for its reply, returning the ticket
    /// the reply is reaped by.
    ///
    /// # Errors
    ///
    /// As [`call`](Self::call), and [`Errno::WouldBlock`] while this process
    /// holds as many calls at the endpoint as its depth there allows.
    fn post(&mut self, request: &[u8]) -> Result<u64, Errno>;

    /// The reply to the request posted as `ticket`, its length written into
    /// `reply`, or `None` while it is still owed. A reaped reply is retired.
    ///
    /// # Errors
    ///
    /// As [`call`](Self::call).
    fn reap(&mut self, ticket: u64, reply: &mut [u8]) -> Result<Option<usize>, Errno>;

    /// Wait for the reply to the request posted as `ticket`, its length
    /// written into `reply`. Only that request's answer ends the wait.
    ///
    /// # Errors
    ///
    /// As [`call`](Self::call).
    fn await_reply(&mut self, ticket: u64, reply: &mut [u8]) -> Result<usize, Errno>;

    /// A region a payload crosses to or from the session, unmapped when
    /// dropped.
    type Region: PayloadRegion;

    /// A fresh `len`-byte region granted to the session, and the grant's
    /// handle.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when the kernel refuses the region or its grant.
    fn grant_region(&mut self, len: usize) -> Result<(Self::Region, u64), Errno>;
}

/// The bytes of a region a payload crosses ([`WindowTransport::Region`]).
pub trait PayloadRegion {
    /// The region's bytes.
    fn bytes_mut(&mut self) -> &mut [u8];
}

/// Step the drop numbered `serial` onto the desktop that window `window_id`'s
/// drag concluded there, over `transport`, waiting for the desktop to take
/// it: what the thread carrying the drop out calls around each item, so the
/// desktop knows a name before its first write.
///
/// # Errors
///
/// The desktop's refusal — [`Errno::NotFound`] once the drop has ended there
/// or its window has gone, [`Errno::OutOfRange`] for a step out of turn — a
/// transport failure, or a corrupt status frame.
pub fn step_landing<T: WindowTransport>(
    transport: &mut T,
    window_id: u64,
    serial: u32,
    step: LandingStep,
) -> Result<(), Errno> {
    let mut frame = [0u8; DROP_LANDING_MAX_WIRE_LEN];
    let len = WindowRequest::DropLanding {
        window_id,
        serial,
        step,
    }
    .encode(&mut frame)?;
    let mut reply = [0u8; STATUS_REPLY_LEN];
    let got = transport.call(&frame[..len], &mut reply)?;
    decode_status_reply(reply.get(..got).ok_or(Errno::BufferTooSmall)?)
}

/// A request the client posts rather than waits on: every request a loop
/// makes, so no loop waits on the session's for an answer.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PostedRequest {
    /// [`WindowClient::resize`].
    Resize,
    /// [`WindowClient::set_cursor`].
    Cursor,
    /// [`WindowClient::set_keyboard_mode`].
    KeyboardMode,
    /// [`WindowClient::lock_pointer`].
    PointerLock,
    /// [`WindowClient::set_tooltip`].
    Tooltip,
    /// [`WindowClient::drag_verdict`].
    DragVerdict,
    /// [`WindowClient::answer_save_offer`].
    SaveOfferAnswer,
    /// [`WindowClient::answer_items_offer`].
    ItemsOfferAnswer,
    /// [`WindowClient::answer_file_offer`].
    FileOfferAnswer,
    /// [`WindowClient::set_title`].
    Title,
    /// [`WindowClient::set_sizing`].
    Sizing,
    /// [`WindowClient::present`].
    Present,
    /// [`WindowClient::end_landing`], for the drop it numbers: each drop's
    /// end is its own, never folded into another's.
    LandingEnded(u32),
    /// [`WindowClient::ask_offered_file`], for the offer it asks about: a
    /// newer ask over the same window replaces one not yet sent.
    OfferedFile(u32),
    /// A window of any kind or a layer surface created: refused, it never
    /// existed.
    Create,
    /// [`WindowClient::open_menu`], for the open it names.
    OpenMenu(u64),
    /// [`WindowClient::restate_menu`], for the open it restates.
    RestateMenu(u64),
    /// [`WindowClient::open_alert`], for the alert it names.
    OpenAlert(u64),
    /// [`WindowClient::close`].
    Close,
    /// [`WindowClient::set_size_state`].
    SizeState,
    /// [`WindowClient::pick_file`].
    PickFile,
    /// [`WindowClient::begin_drag`].
    BeginDrag,
    /// [`WindowClient::place_dropped`].
    PlaceDropped,
    /// [`WindowClient::save_into`], for the offer it answers.
    SaveInto(u32),
    /// [`WindowClient::authorise`].
    Authorise,
    /// [`WindowClient::close_alert`], for the alert it withdraws.
    CloseAlert(u64),
    /// [`WindowClient::reveal_document`], answered by the token it
    /// returned.
    RevealDocument(u64),
    /// [`WindowClient::activate_window`].
    Activate,
    /// [`WindowClient::set_clipboard`].
    SetClipboard,
    /// [`WindowClient::set_app_bar`].
    AppBar,
    /// [`WindowClient::set_backdrop_blur`].
    BackdropBlur,
    /// [`WindowClient::place_layer`].
    PlaceLayer,
    /// [`WindowClient::hand_over_launch`], answered by the token it
    /// returned.
    HandOver(u64),
    /// [`WindowClient::ask_open_targets`].
    OpenTarget,
    /// [`WindowClient::ask_menu_text`], for the open it asks about.
    MenuText(u64),
    /// [`WindowClient::ask_app_bar_text`], for the row and revision it asks
    /// about.
    AppBarText(AppMenuItemId, u32),
    /// [`WindowClient::ask_drag_spot`], for the report it asks about: a newer
    /// ask over the same window replaces one not yet sent.
    DragSpot(u32),
    /// [`WindowClient::ask_drop_target`].
    DropTarget,
    /// [`WindowClient::ask_picked_name`].
    PickedName,
    /// [`WindowClient::ask_picked_files`].
    PickedFile,
    /// [`WindowClient::ask_offered_items`], a page of the drag it reads.
    OfferedItems(u32),
    /// [`WindowClient::ask_clipboard`].
    Clipboard,
    /// [`WindowClient::ask_terrain`].
    Terrain,
}

impl PostedRequest {
    /// Whether a newer request of this kind supersedes an older one about
    /// the same object, so at most one is in flight and the newest is held.
    const fn supersedes(self) -> bool {
        matches!(
            self,
            Self::Resize
                | Self::Cursor
                | Self::KeyboardMode
                | Self::PointerLock
                | Self::Tooltip
                | Self::DragVerdict
                | Self::SaveOfferAnswer
                | Self::ItemsOfferAnswer
                | Self::FileOfferAnswer
                | Self::Title
                | Self::Sizing
                | Self::Present
                | Self::OfferedFile(_)
                | Self::SizeState
                | Self::BackdropBlur
                | Self::PlaceLayer
                | Self::DragSpot(_)
                | Self::Terrain
                | Self::OpenTarget
                | Self::PickedFile
                | Self::AppBar
                | Self::RestateMenu(_)
        )
    }

    /// Whether an ask of kind `asked` is held behind an unanswered one of
    /// this kind over the same object, as the newest wanted.
    fn folds(self, asked: Self) -> bool {
        self.supersedes()
            && match (self, asked) {
                (Self::OfferedFile(_), Self::OfferedFile(_))
                | (Self::DragSpot(_), Self::DragSpot(_)) => true,
                _ => self == asked,
            }
    }

    const fn what(self) -> &'static str {
        match self {
            Self::Resize => "a resize",
            Self::Cursor => "a pointer shape",
            Self::KeyboardMode => "a way of reading the keyboard",
            Self::PointerLock => "locking the pointer",
            Self::Tooltip => "a tooltip",
            Self::DragVerdict => "a drop answer",
            Self::SaveOfferAnswer => "a save answer",
            Self::ItemsOfferAnswer => "a drop answer for the desktop's items",
            Self::FileOfferAnswer => "a drop answer for a file offered",
            Self::Title => "a title",
            Self::Sizing => "a resize range",
            Self::Present => "a frame",
            Self::LandingEnded(_) => "a drop's end",
            Self::OfferedFile(_) => "a file offer's media type",
            Self::Create => "a window",
            Self::OpenMenu(_) => "a menu",
            Self::RestateMenu(_) => "restating a menu",
            Self::OpenAlert(_) => "an alert",
            Self::Close => "a window's closing",
            Self::SizeState => "a window's size",
            Self::PickFile => "a file pick",
            Self::BeginDrag => "a drag",
            Self::PlaceDropped => "a dropped file",
            Self::SaveInto(_) => "a save",
            Self::Authorise => "an authorisation",
            Self::CloseAlert(_) => "an alert's withdrawal",
            Self::RevealDocument(_) => "showing a document",
            Self::Activate => "bringing a window forward",
            Self::SetClipboard => "a copy",
            Self::AppBar => "the icon bar's entry",
            Self::BackdropBlur => "a backdrop blur",
            Self::PlaceLayer => "a layer's place",
            Self::HandOver(_) => "a hand-over",
            Self::OpenTarget => "a target to open",
            Self::MenuText(_) | Self::AppBarText(..) => "a menu's text",
            Self::DragSpot(_) => "a drag's place",
            Self::DropTarget => "a drop's target",
            Self::PickedName => "a picked name",
            Self::PickedFile => "a picked file",
            Self::OfferedItems(_) => "a drag's items",
            Self::Clipboard => "a paste",
            Self::Terrain => "the desktop's terrain",
        }
    }
}

/// A file a folder pick delegated, as [`Answer::PickedFile`] lands it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Picked {
    /// The `fd_redeem` handle minted to this application; never zero.
    pub handle: u64,
    /// The file's own name, for the application to show it by.
    pub name: String,
}

/// The application a drag was dropped on, as [`Answer::DropTarget`] lands
/// it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dropped {
    /// The target bundle's entry binary.
    pub run_path: String,
    /// Whether its signed manifest claims to edit the documents it opens.
    pub writes_documents: bool,
}

/// An item a drag offers, as [`Answer::OfferedItems`] lands it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfferedPath {
    /// Its absolute path.
    pub path: String,
    /// The node the desktop's listing showed there, or [`FileId::NONE`].
    pub id: FileId,
}

/// What a request the application asked for lands as, for its loop to take
/// ([`WindowClient::take_answer`]).
#[derive(Debug)]
pub enum Answer {
    /// A file offer's media type ([`WindowClient::ask_offered_file`]).
    OfferedFile(OfferedMedia),
    /// The next target queued for the application to open, or `None` once
    /// none is left ([`WindowClient::ask_open_targets`]).
    OpenTarget(Result<Option<Target>, Errno>),
    /// The text committed into a menu open's quick-entry field
    /// ([`WindowClient::ask_menu_text`]).
    MenuText {
        /// The window the chain belonged to.
        window_id: u64,
        /// The open asked about.
        open_id: u64,
        /// The text, or `None` for none committed.
        text: Result<Option<String>, Errno>,
    },
    /// The text committed into an icon-bar row's field
    /// ([`WindowClient::ask_app_bar_text`]).
    AppBarText {
        /// The row.
        item: AppMenuItemId,
        /// The revision the row answered under.
        revision: u32,
        /// The text, or `None` for none committed.
        text: Result<Option<String>, Errno>,
    },
    /// The desktop folder a drag's report names
    /// ([`WindowClient::ask_drag_spot`]).
    DragSpot {
        /// The window whose drag it is.
        window_id: u64,
        /// The report asked about.
        serial: u32,
        /// The folder.
        folder: Result<String, Errno>,
    },
    /// The application a drag was dropped on
    /// ([`WindowClient::ask_drop_target`]).
    DropTarget {
        /// The window whose drag it was.
        window_id: u64,
        /// The target.
        target: Result<Dropped, Errno>,
    },
    /// The name of the file a pick chose ([`WindowClient::ask_picked_name`]).
    PickedName {
        /// The window that picked.
        window_id: u64,
        /// The name.
        name: Result<String, Errno>,
    },
    /// The next file a folder pick delegated
    /// ([`WindowClient::ask_picked_files`]).
    PickedFile {
        /// The window that picked.
        window_id: u64,
        /// The file, or [`Errno::NotFound`] once none is left.
        file: Result<Picked, Errno>,
    },
    /// Every item a drag offers, in order
    /// ([`WindowClient::ask_offered_items`]).
    OfferedItems {
        /// The window the drag is offered to.
        window_id: u64,
        /// The drag.
        drag: u32,
        /// Its items.
        items: Result<Vec<OfferedPath>, Errno>,
    },
    /// What the clipboard holds, `None` when it is empty
    /// ([`WindowClient::ask_clipboard`]).
    Clipboard {
        /// The window that asked.
        window_id: u64,
        /// What it holds.
        contents: Result<Option<(ClipboardKind, Vec<u8>)>, Errno>,
    },
    /// The visible windows' rectangles, back to front
    /// ([`WindowClient::ask_terrain`]).
    Terrain {
        /// The layer surface that asked.
        window_id: u64,
        /// The plates.
        plates: Result<Vec<TerrainPlate>, Errno>,
    },
    /// Whether a document was shown in its folder
    /// ([`WindowClient::reveal_document`]).
    Revealed {
        /// The token the reveal was answered by.
        token: u64,
        /// Whether the desktop showed it.
        shown: Result<(), Errno>,
    },
    /// How a hand-over went ([`WindowClient::hand_over_launch`]).
    HandOver {
        /// The token the hand-over was answered by.
        token: u64,
        /// Whether a live instance took it.
        outcome: Result<HandOverOutcome, Errno>,
    },
    /// The window's last present was answered, so a paint deferred behind it
    /// may go now ([`WindowClient::owe_frame`]).
    FrameReturned {
        /// The window.
        window_id: u64,
    },
}

/// The media type [`WindowClient::ask_offered_file`] asked for, as the
/// session answered it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfferedMedia {
    /// The window the offer was over.
    pub window_id: u64,
    /// The offer asked about.
    pub serial: u32,
    /// The offered file's media type: [`Errno::NotFound`] once the offer no
    /// longer stood, or how the read failed.
    pub media: Result<String, Errno>,
}

/// A posted request the session refused, handed to the application once its
/// answer is reaped ([`EventError::Refused`]).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Refused {
    /// The window it was for.
    pub window_id: u64,
    /// What it asked.
    pub request: PostedRequest,
    /// The answer.
    pub errno: Errno,
}

impl fmt::Display for Refused {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            out,
            "the desktop refused {} ({})",
            self.request.what(),
            self.errno
        )
    }
}

/// What one coalesced request asks, its object apart: no more than each
/// kind carries, so holding the newest costs a few bytes rather than a whole
/// request frame. A request that reads on from an answer is spelled here too.
#[derive(Copy, Clone, Debug)]
enum Asked {
    Cursor(CursorShape),
    KeyboardMode(KeyboardMode),
    PointerLock(bool),
    Tooltip(WindowRegion, TooltipText),
    DragVerdict(u32, Option<DropOperation>),
    SaveOfferAnswer(u32, bool),
    ItemsOfferAnswer(u32, Option<DropOperation>),
    FileOfferAnswer(u32, bool),
    Title(WindowTitle),
    Sizing(WindowSizing),
    SizeState(WindowSizeState),
    BackdropBlur(u16),
    PlaceLayer(i32, i32, LayerDepth),
    Present(u32, DamageList),
    OfferedFile(u32),
    DragSpot(u32),
    Terrain,
    OpenTarget,
    PickedFile,
    /// A drag's items, read on from the item numbered second.
    OfferedItems(u32, u32),
    /// A paste offered the region granted as this handle.
    Clipboard(u64),
}

impl Asked {
    const fn kind(&self) -> PostedRequest {
        match self {
            Self::Cursor(_) => PostedRequest::Cursor,
            Self::KeyboardMode(_) => PostedRequest::KeyboardMode,
            Self::PointerLock(_) => PostedRequest::PointerLock,
            Self::Tooltip(..) => PostedRequest::Tooltip,
            Self::DragVerdict(..) => PostedRequest::DragVerdict,
            Self::SaveOfferAnswer(..) => PostedRequest::SaveOfferAnswer,
            Self::ItemsOfferAnswer(..) => PostedRequest::ItemsOfferAnswer,
            Self::FileOfferAnswer(..) => PostedRequest::FileOfferAnswer,
            Self::Title(_) => PostedRequest::Title,
            Self::Sizing(_) => PostedRequest::Sizing,
            Self::SizeState(_) => PostedRequest::SizeState,
            Self::BackdropBlur(_) => PostedRequest::BackdropBlur,
            Self::PlaceLayer(..) => PostedRequest::PlaceLayer,
            Self::Present(..) => PostedRequest::Present,
            Self::OfferedFile(serial) => PostedRequest::OfferedFile(*serial),
            Self::DragSpot(serial) => PostedRequest::DragSpot(*serial),
            Self::Terrain => PostedRequest::Terrain,
            Self::OpenTarget => PostedRequest::OpenTarget,
            Self::PickedFile => PostedRequest::PickedFile,
            Self::OfferedItems(drag, _) => PostedRequest::OfferedItems(*drag),
            Self::Clipboard(_) => PostedRequest::Clipboard,
        }
    }

    const fn request(self, window_id: u64) -> WindowRequest {
        match self {
            Self::Cursor(shape) => WindowRequest::SetCursor { window_id, shape },
            Self::KeyboardMode(mode) => WindowRequest::SetKeyboardMode { window_id, mode },
            Self::PointerLock(locked) => WindowRequest::LockPointer { window_id, locked },
            Self::Tooltip(region, text) => WindowRequest::SetTooltip {
                window_id,
                region,
                text,
            },
            Self::DragVerdict(serial, verdict) => WindowRequest::DragVerdict {
                window_id,
                serial,
                verdict,
            },
            Self::ItemsOfferAnswer(serial, verdict) => WindowRequest::ItemsOfferAnswer {
                window_id,
                serial,
                verdict,
            },
            Self::SaveOfferAnswer(serial, accepts) => WindowRequest::SaveOfferAnswer {
                window_id,
                serial,
                accepts,
            },
            Self::FileOfferAnswer(serial, places) => WindowRequest::FileOfferAnswer {
                window_id,
                serial,
                places,
            },
            Self::Title(title) => WindowRequest::SetTitle { window_id, title },
            Self::Sizing(sizing) => WindowRequest::SetSizing { window_id, sizing },
            Self::SizeState(state) => WindowRequest::SetSizeState { window_id, state },
            Self::BackdropBlur(radius_px) => WindowRequest::SetBackdropBlur {
                window_id,
                radius_px,
            },
            Self::PlaceLayer(x, y, depth) => WindowRequest::PlaceLayer {
                window_id,
                x,
                y,
                depth,
            },
            Self::Present(frame_index, damage) => WindowRequest::Present {
                window_id,
                frame_index,
                damage,
            },
            Self::OfferedFile(serial) => WindowRequest::OfferedFile { window_id, serial },
            Self::DragSpot(serial) => WindowRequest::QueryDragSpot { window_id, serial },
            Self::Terrain => WindowRequest::TakeTerrain { window_id },
            Self::OpenTarget => WindowRequest::TakeOpenTarget,
            Self::PickedFile => WindowRequest::TakePickedFile { window_id },
            Self::OfferedItems(drag, from) => WindowRequest::OfferedItems {
                window_id,
                drag,
                from,
            },
            Self::Clipboard(shm_handle) => WindowRequest::GetClipboard {
                window_id,
                shm_handle,
            },
        }
    }
}

/// A request waiting to be posted.
enum Held {
    /// A coalesced request, or one reading on from an answer.
    Asked(Asked),
    /// Any other request, as encoded: its kind is its posting's.
    Frame(Vec<u8>),
}

impl Held {
    /// The kind the request is, posted for a posting of `kind`.
    const fn kind(&self, kind: PostedRequest) -> PostedRequest {
        match self {
            Self::Asked(asked) => asked.kind(),
            Self::Frame(_) => kind,
        }
    }
}

/// What a posting keeps until its answer lands.
enum Carried<R> {
    Nothing,
    /// The region a copy or a paste crosses, kept mapped until the session
    /// has answered, and whether a paste's region is its second offer.
    Region(R, bool),
    /// The items of a drag read so far.
    Items(Vec<OfferedPath>),
}

/// A posted request awaiting its answer, or one waiting for room.
struct Posting<R> {
    /// The window it is for, or zero for this process's own.
    window_id: u64,
    kind: PostedRequest,
    /// `None` while the request waits for room: this process's own calls
    /// fill its place at the endpoint, and one of them being answered is what
    /// makes room, so it is posted then, in the order it was asked.
    ticket: Option<u64>,
    /// What goes in its place: the request itself while it waits for room,
    /// or the newest of its kind asked for since it was posted.
    held: Option<Held>,
    carried: Carried<R>,
    /// Its window was closed since: the answer is still reaped, but nobody
    /// is told of it.
    orphaned: bool,
}

/// A request reading on from an answer, in its posting's place.
#[derive(Copy, Clone, Debug)]
enum ReadOn {
    OpenTarget,
    PickedFile,
    /// A drag's items, from the item numbered second.
    OfferedItems(u32, u32),
    /// A paste offered the region granted as this handle.
    Clipboard(u64),
}

impl ReadOn {
    const fn asked(self) -> Asked {
        match self {
            Self::OpenTarget => Asked::OpenTarget,
            Self::PickedFile => Asked::PickedFile,
            Self::OfferedItems(drag, from) => Asked::OfferedItems(drag, from),
            Self::Clipboard(shm_handle) => Asked::Clipboard(shm_handle),
        }
    }
}

/// What an answer came to.
enum Outcome {
    /// A request answered with a status.
    Status(Result<(), Errno>),
    /// What lands for the application, if anything, and the request reading
    /// on in its posting's place.
    Read(Option<Answer>, Option<ReadOn>),
}

impl Outcome {
    const fn landed(answer: Answer) -> Self {
        Self::Read(Some(answer), None)
    }
}

/// What a window last presented, so the client can re-present it when
/// the session asks for a redraw.
///
/// The frame index alone is not enough: full-window damage needs the
/// window's current client extent, which the create/resize calls are the
/// only place that knows.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct LastPresent {
    /// The window the record belongs to.
    window_id: u64,
    /// The window it is a transient of, which the session closes it with.
    owner: Option<u64>,
    /// Client width in pixels, as last created or resized.
    width_px: u32,
    /// Client height in pixels, as last created or resized.
    height_px: u32,
    /// The frame index of the last accepted present, if the window has
    /// presented at all since it was created or resized.
    frame_index: Option<u32>,
    /// The extent the session last accepted, which a refused resize leaves
    /// the window at.
    accepted: (u32, u32),
    /// The newest geometry the session reported for the window.
    reported: Option<(u32, u32, WindowSizeState)>,
    /// A resize was asked while another was unanswered; the reported
    /// geometry is owed back once that one is.
    deferred: bool,
    /// Where the window's frames stand with the session.
    frames: Frames,
    /// A present the session refused never reached the screen, so the next
    /// one declares the whole window.
    owed_whole: bool,
    /// A paint was deferred behind the present in flight, so the window is
    /// owed [`Answer::FrameReturned`] once that one is answered.
    frame_owed: bool,
}

/// Where a window's frames stand with the session.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Frames {
    /// The session holds them.
    Mapped,
    /// The session released its side, so the next present re-attaches a
    /// region and sends the whole window.
    Released,
    /// Released because nobody can see the window, and not yet asked for
    /// again.
    Hidden,
}

impl LastPresent {
    /// What the window is owed once a resize that had to wait may go: the
    /// newest size the session reported, to lay out at, or, for a re-attach
    /// of a window it never sized, its paint asked for again.
    fn deferred_event(&self) -> Option<WindowEvent> {
        match self.reported {
            Some((width_px, height_px, state)) => Some(WindowEvent::Resized {
                window_id: self.window_id,
                width_px,
                height_px,
                state,
            }),
            None if self.frames == Frames::Released => Some(WindowEvent::RedrawRequested {
                window_id: self.window_id,
            }),
            None => None,
        }
    }
}

/// Where a window's presents stand ([`WindowClient::present_state`]).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PresentState {
    /// None is unanswered: the frame the session is not reading is the
    /// application's to draw.
    Idle,
    /// One is unanswered, so both frames are the session's: the next paint
    /// waits for [`Answer::FrameReturned`] ([`WindowClient::owe_frame`]).
    InFlight,
    /// One waits for room, naming this frame, which the session has not yet
    /// been sent: the frame may be drawn again and presented again, and the
    /// two presents go as one.
    Waiting(u32),
}

/// A typed handle on the desktop session's window service.
///
/// Every request is posted: an interactive loop never waits on the session,
/// whose own loop may be a frame or more from serving it. The client keeps
/// each request until its answer lands, taken in by the event drain, and
/// hands the application what it asked for as an [`Answer`] and what the
/// session refused as an [`EventError::Refused`]. It remembers each of its
/// windows' extent and last presented frame so it can answer a
/// [`WindowEvent::RedrawRequested`] on the app's behalf — see
/// [`WindowEvents::wait`].
pub struct WindowClient<T: WindowTransport> {
    transport: T,
    presented: Vec<LastPresent>,
    /// The scratch every request is encoded into, held once for the life of
    /// the client rather than taken per call: it is sized to the widest
    /// operation the channel has, so a per-call array would cost a present
    /// — the hottest operation and one of the shortest — the whole of the
    /// widest one's clearing.
    frame: [u8; WindowRequest::MAX_WIRE_LEN],
    /// The scratch every reply is read into, a call's or a posted request's,
    /// held once for the same reason [`Self::frame`] is.
    reply: [u8; REPLY_MAX],
    /// The serving session's attested identity, as the connect stated it.
    session: Option<ProcId>,
    /// The range this client names what it creates by.
    ids: Option<ObjectIds>,
    /// The requests posted and not yet answered, and those waiting for room,
    /// in the order asked.
    posted: Vec<Posting<T::Region>>,
    /// Refusals reaped and not yet handed to the application.
    refused: VecDeque<Refused>,
    /// Geometry the application is owed again: a resize it asked while
    /// another was unanswered, now free to go, or the size the session kept
    /// on refusing one.
    owed: VecDeque<WindowEvent>,
    /// Answers landed and not yet taken by the application.
    answers: VecDeque<Answer>,
    /// An answer landed since a wait last returned for one.
    unannounced: bool,
    /// The token the next hand-over or reveal is answered by.
    next_token: u64,
    /// The session's endpoint has gone: nothing posted will be answered.
    lost: bool,
    /// The client could not keep what it was owed: the application ends the
    /// channel rather than wait on an answer that will never land.
    broken: Option<Errno>,
}

impl<T: WindowTransport> WindowClient<T> {
    /// A client over `transport`, not yet connected.
    pub const fn new(transport: T) -> Self {
        Self {
            transport,
            presented: Vec::new(),
            frame: [0; WindowRequest::MAX_WIRE_LEN],
            reply: [0; REPLY_MAX],
            session: None,
            ids: None,
            posted: Vec::new(),
            refused: VecDeque::new(),
            owed: VecDeque::new(),
            answers: VecDeque::new(),
            unannounced: false,
            next_token: 1,
            lost: false,
            broken: None,
        }
    }

    /// Connect to the session: learn the desktop this client's windows are
    /// displayed on — the screen extent, the UI scale and the active
    /// appearance — the serving session's identity, and the range this client
    /// names what it creates by.
    ///
    /// An app connects **before** it creates anything, so its first window is
    /// sized to a screen it knows and its first frame is painted at the right
    /// density in the right colours. Thereafter the session publishes each new
    /// state on the desktop system notice; [`Desktop`](crate::Desktop) holds
    /// the answer and [`adopt`](crate::Desktop::adopt) keeps it current. It is
    /// the one call an application makes of the session, made before it owes
    /// anyone a frame.
    ///
    /// # Errors
    ///
    /// [`Errno::AlreadyExists`] for a client already connected, whose range
    /// stands; the session's refusal ([`Errno::LimitExceeded`] for a process
    /// holding as many ranges as it may, or a session tearing down that no
    /// longer has a screen to describe), a transport failure, or a corrupt
    /// reply — never a guessed extent.
    pub fn connect(&mut self) -> Result<DesktopInfo, Errno> {
        if self.ids.is_some() {
            return Err(Errno::AlreadyExists);
        }
        let len = self.call_reply(&WindowRequest::Connect)?;
        let connected = decode_connect_reply(self.reply.get(..len).ok_or(Errno::BufferTooSmall)?)?;
        self.session = Some(connected.server);
        self.ids = Some(connected.ids);
        Ok(connected.desktop)
    }

    /// The serving session's kernel-attested [`ProcId`], once
    /// [`connect`](Self::connect) has learned it — the identity an app
    /// requires of every event's sender.
    ///
    /// `None` before then: until the session has said who it is, an app has
    /// nothing to authenticate against and must accept nothing (fail closed).
    #[must_use]
    pub const fn session(&self) -> Option<ProcId> {
        self.session
    }

    /// The next id this client names an object by.
    fn mint(&mut self) -> Result<u64, Errno> {
        self.ids
            .as_mut()
            .ok_or(Errno::NotConnected)?
            .mint()
            .ok_or(Errno::NoSpace)
    }

    /// Open a window: `frame_count` frames shaped as `surface`, laid out
    /// back-to-back in the region granted as `shm_handle`, titled
    /// `title`, with this window's events delivered to the app's own
    /// `event_endpoint`.
    ///
    /// `sizing` says whether the window manager presents the window with a
    /// resize grabber and a live maximize/restore size toggle: a fixed-size
    /// app passes [`WindowSizing::Fixed`] and is offered neither affordance
    /// (and never receives a [`WindowEvent::Resized`]), while a
    /// [`WindowSizing::Resizable`] app re-lays-out to each reported size
    /// and re-maps its region with [`Self::resize`].
    ///
    /// A resizable app declares its smallest workable client in that
    /// variant, once (`0` for no minimum of its own). The **window
    /// manager** enforces it and will not resize below it, so the app lays
    /// out at exactly the size it is told; an app that resized itself back
    /// up instead would fight the drag, frame by frame.
    ///
    /// The size is the app's own choice; [`Self::connect`] is how it learns
    /// the screen it must fit on before making that choice.
    ///
    /// Returns the window's id, named by this client from its own range, so
    /// the window's title, first present and anything else asked of it go
    /// straight after the create, served in that order. A refusal arrives
    /// later as an [`EventError::Refused`] naming the window, which then never
    /// existed: what was asked of it meanwhile is refused with it, unreported.
    ///
    /// # Errors
    ///
    /// * [`Errno::LengthOutOfRange`] / [`Errno::OutOfRange`] — a title
    ///   or geometry the protocol refuses, caught before any request.
    /// * [`Errno::NotConnected`] before [`Self::connect`], and
    ///   [`Errno::NoSpace`] once the client's range is spent.
    /// * A transport failure.
    ///
    /// [`WindowEvent::Resized`]: tairix_abi::window_ipc::WindowEvent::Resized
    pub fn create(
        &mut self,
        shm_handle: u64,
        event_endpoint: u64,
        frame_count: u32,
        surface: &DisplayMode,
        title: &str,
        sizing: WindowSizing,
    ) -> Result<u64, Errno> {
        let title = WindowTitle::new(title)?;
        let window_id = self.mint()?;
        let request = WindowRequest::Create {
            window_id,
            shm_handle,
            event_endpoint,
            frame_count,
            width_px: surface.width_px,
            height_px: surface.height_px,
            stride_bytes: surface.stride_bytes,
            format: surface.format,
            title,
            sizing,
        };
        self.open(window_id, None, &request, surface)
    }

    /// Open an undecorated popup surface stacked directly above this app's
    /// own window `spec.parent_window_id`, as `spec` describes it.
    ///
    /// A popup is how an app draws a context menu or a settings sheet that
    /// must not be clipped by the bounds of the window that owns it: the
    /// session resolves the parent's current screen position, adds
    /// `spec.offset_x`/`spec.offset_y`, and clamps the whole popup onto the
    /// screen (an app is never told its own window's screen position). The
    /// popup is undecorated — no title bar, no frame furniture — and is
    /// never listed on the taskbar. It counts against the same per-client
    /// window budget as [`Self::create`], so a popup cannot be used to
    /// exceed the cap.
    ///
    /// Answered as [`Self::create`] is, and thereafter [`Self::present`] and
    /// [`Self::close`] act on the popup's id exactly as they do for a
    /// top-level window. The session's refusal — a foreign or unknown parent,
    /// the per-client window budget reached — arrives as the create's does.
    ///
    /// # Errors
    ///
    /// [`Self::create`]'s.
    pub fn create_popup(&mut self, spec: &PopupSpec) -> Result<u64, Errno> {
        let transient = &spec.transient;
        let window_id = self.mint()?;
        let request = WindowRequest::CreatePopup {
            window_id,
            parent_window_id: transient.parent_window_id,
            shm_handle: transient.shm_handle,
            event_endpoint: transient.event_endpoint,
            frame_count: transient.frame_count,
            width_px: transient.surface.width_px,
            height_px: transient.surface.height_px,
            stride_bytes: transient.surface.stride_bytes,
            format: transient.surface.format,
            offset_x: spec.offset_x,
            offset_y: spec.offset_y,
        };
        self.open(
            window_id,
            Some(transient.parent_window_id),
            &request,
            &transient.surface,
        )
    }

    /// Open a tool window: a floating palette hung from this app's own
    /// top-level window `spec.transient.parent_window_id`, framed by the
    /// window manager with a mini title band reading `spec.title`, moved by
    /// the user, and reported to this app as it moves
    /// ([`WindowEvent::ToolMoved`]).
    ///
    /// It is placed and lives as [`Self::create_popup`]'s popup does —
    /// relative to the parent's client origin, above the parent, closed and
    /// hidden with it, off the taskbar — except that `spec.carry` asks for
    /// the press the parent still holds to carry it, and it takes the
    /// keyboard only when the user presses it. A `spec.sizing` the user may
    /// resize it within gives it resize edges — never a maximize — and each
    /// new client size arrives as [`WindowEvent::Resized`], as a window's
    /// does. Answered as [`Self::create`] is; the session refuses a parent
    /// that is itself a transient [`Errno::NotSupported`].
    ///
    /// # Errors
    ///
    /// [`Self::create`]'s, and [`Errno::OutOfRange`] for a carry off the band
    /// or a range naming no reachable size, caught before any request.
    pub fn create_tool(&mut self, spec: &ToolSpec) -> Result<u64, Errno> {
        let transient = &spec.transient;
        let window_id = self.mint()?;
        let request = WindowRequest::CreateTool {
            window_id,
            parent_window_id: transient.parent_window_id,
            shm_handle: transient.shm_handle,
            event_endpoint: transient.event_endpoint,
            frame_count: transient.frame_count,
            width_px: transient.surface.width_px,
            height_px: transient.surface.height_px,
            stride_bytes: transient.surface.stride_bytes,
            format: transient.surface.format,
            offset_x: spec.offset_x,
            offset_y: spec.offset_y,
            carry: spec.carry,
            title: spec.title,
            sizing: spec.sizing,
        };
        self.open(
            window_id,
            Some(transient.parent_window_id),
            &request,
            &transient.surface,
        )
    }

    /// Open a dialog window: a form, a question or an operation's progress,
    /// drawn by this app and owned by its own top-level window
    /// `spec.transient.parent_window_id`.
    ///
    /// The session frames it as a dialog whose band names this application by
    /// its attested identity before `spec.title`, centres it over its owner
    /// and keeps it on screen, and stacks, hides and closes it with its owner,
    /// off the taskbar. It takes the keyboard only where this app has earned
    /// the front, and ignores presses until it has been seen a moment. An
    /// [`OwnerModal`](tairix_abi::window_ipc::DialogModality::OwnerModal)
    /// dialog holds its owner as an alert does; a modeless one holds nothing.
    /// The extent is this app's to size to its content: one past the screen is
    /// fitted to it and told so with a [`WindowEvent::Resized`]. Answered as
    /// [`Self::create`] is; the session refuses an owner that is itself a
    /// transient [`Errno::NotSupported`].
    ///
    /// # Errors
    ///
    /// [`Self::create`]'s.
    pub fn create_dialog(&mut self, spec: &DialogSpec) -> Result<u64, Errno> {
        let transient = &spec.transient;
        let window_id = self.mint()?;
        let request = WindowRequest::CreateDialog {
            window_id,
            owner_window_id: transient.parent_window_id,
            shm_handle: transient.shm_handle,
            event_endpoint: transient.event_endpoint,
            frame_count: transient.frame_count,
            width_px: transient.surface.width_px,
            height_px: transient.surface.height_px,
            stride_bytes: transient.surface.stride_bytes,
            format: transient.surface.format,
            modality: spec.modality,
            title: spec.title,
        };
        self.open(
            window_id,
            Some(transient.parent_window_id),
            &request,
            &transient.surface,
        )
    }

    /// Open a desktop layer surface: an undecorated surface placed in
    /// **screen** coordinates, in the desktop's own stacking layers rather
    /// than inside a window of this app's.
    ///
    /// The one request on this channel that requires `CAP_DESKTOP_LAYER`, and
    /// the difference between it and [`Self::create_popup`]: a popup is
    /// also undecorated, but it hangs off a window this app already owns
    /// and is offset from that window's client origin, so it tells the app
    /// nothing about the screen. This names an absolute point and a
    /// stacking position relative to other applications' windows.
    ///
    /// The session bounds what it hands back: the surface is clamped onto
    /// the work area, never enters the focus rotation, never receives a
    /// key, catches the pointer only where its own content is opaque, and
    /// is hidden whenever a trusted surface is up. Each side is at most
    /// `DESKTOP_LAYER_MAX_SIDE_LOGICAL` logical pixels.
    ///
    /// Answered as [`Self::create`] is, and thereafter [`Self::present`] and
    /// [`Self::close`] act on the surface's id exactly as they do for a
    /// window. The refusal of a caller without `CAP_DESKTOP_LAYER`
    /// ([`Errno::PermissionDenied`]) or of one already holding a layer
    /// surface ([`Errno::LimitExceeded`]) arrives as the create's does, and an
    /// app degrades over it rather than dying: the refusal is the answer, not
    /// a fault.
    ///
    /// # Errors
    ///
    /// [`Self::create`]'s, with a geometry beyond the bound refused before any
    /// request.
    pub fn open_layer(&mut self, spec: &LayerSpec) -> Result<u64, Errno> {
        let window_id = self.mint()?;
        let request = WindowRequest::OpenLayer {
            window_id,
            shm_handle: spec.shm_handle,
            event_endpoint: spec.event_endpoint,
            frame_count: spec.frame_count,
            width_px: spec.surface.width_px,
            height_px: spec.surface.height_px,
            stride_bytes: spec.surface.stride_bytes,
            format: spec.surface.format,
            x: spec.x,
            y: spec.y,
            depth: spec.depth,
        };
        self.open(window_id, None, &request, &spec.surface)
    }

    /// Post the create `request`, which names `window_id`, a transient of
    /// `owner` or a window of its own, and record the extent it opens at.
    fn open(
        &mut self,
        window_id: u64,
        owner: Option<u64>,
        request: &WindowRequest,
        surface: &DisplayMode,
    ) -> Result<u64, Errno> {
        self.presented
            .try_reserve(1)
            .map_err(|_| Errno::OutOfMemory)?;
        self.post_own(window_id, PostedRequest::Create, request, Carried::Nothing)?;
        self.presented.push(LastPresent {
            window_id,
            owner,
            width_px: surface.width_px,
            height_px: surface.height_px,
            frame_index: None,
            accepted: (surface.width_px, surface.height_px),
            reported: None,
            deferred: false,
            frames: Frames::Mapped,
            owed_whole: false,
            frame_owed: false,
        });
        Ok(window_id)
    }

    /// Move this app's layer surface to a new screen point and stacking
    /// layer. The session clamps the point onto the work area. Its refusal —
    /// a window this app does not own, or one that is not a layer surface —
    /// arrives later as an [`EventError::Refused`].
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn place_layer(
        &mut self,
        window_id: u64,
        x: i32,
        y: i32,
        depth: LayerDepth,
    ) -> Result<(), Errno> {
        self.post_or_hold(window_id, Asked::PlaceLayer(x, y, depth), Carried::Nothing)
    }

    /// Ask for the desktop terrain this app's layer surface sits on: the
    /// visible windows' screen rectangles, back-to-front, landing as
    /// [`Answer::Terrain`].
    ///
    /// Asked for rather than pushed, so a surface that does not care is told
    /// nothing. [`WindowEvent::TerrainChanged`] says an answer would now
    /// differ; asked again while one is unanswered, only the newest is sent.
    ///
    /// # Errors
    ///
    /// A transport failure.
    ///
    /// [`WindowEvent::TerrainChanged`]: tairix_abi::window_ipc::WindowEvent::TerrainChanged
    pub fn ask_terrain(&mut self, window_id: u64) -> Result<(), Errno> {
        self.post_or_hold(window_id, Asked::Terrain, Carried::Nothing)
    }

    /// Present frame `frame_index` of window `window_id`, of which
    /// `damage` changed.
    ///
    /// Posted, so it never waits. The session shows the frame with its next
    /// one, answering at once when the window held no frame it had yet to
    /// show and otherwise once it shows that one; until then it reads both
    /// of the window's frames, so a window has one present in flight and
    /// [`Self::present_state`] says when the next may be drawn. A present the
    /// endpoint has no room for waits for room, and one made while it waits
    /// is sent with it as one. A refusal arrives later as
    /// [`EventError::Refused`], and the window's next present declares the
    /// whole window, since the refused one never reached it.
    ///
    /// # Errors
    ///
    /// [`Errno::WouldBlock`] while a present of the window is in flight — the
    /// window is then owed [`Answer::FrameReturned`] — or a transport failure.
    pub fn present(
        &mut self,
        window_id: u64,
        frame_index: u32,
        damage: DamageList,
    ) -> Result<(), Errno> {
        if self.present_state(window_id) == PresentState::InFlight {
            self.owe_frame(window_id);
            return Err(Errno::WouldBlock);
        }
        let owed = self
            .record_mut(window_id)
            .filter(|record| record.owed_whole)
            .map(|record| (record.width_px, record.height_px));
        let damage = match owed {
            Some((width_px, height_px)) => whole(width_px, height_px)?,
            None => damage,
        };
        self.post_or_hold(
            window_id,
            Asked::Present(frame_index, damage),
            Carried::Nothing,
        )?;
        if let Some(record) = self.record_mut(window_id) {
            record.frame_index = Some(frame_index);
            record.owed_whole = false;
        }
        Ok(())
    }

    /// Where window `window_id`'s presents stand, once every answer that has
    /// landed is taken in: whether the frame the session is not reading may
    /// be drawn, or a paint must wait for [`Answer::FrameReturned`].
    pub fn present_state(&mut self, window_id: u64) -> PresentState {
        self.reap_posted();
        let present = self.posted.iter().find(|posting| {
            posting.window_id == window_id && posting.kind == PostedRequest::Present
        });
        match present {
            None => PresentState::Idle,
            Some(Posting {
                ticket: None,
                held: Some(Held::Asked(Asked::Present(frame_index, _))),
                ..
            }) => PresentState::Waiting(*frame_index),
            Some(_) => PresentState::InFlight,
        }
    }

    /// Owe window `window_id` an [`Answer::FrameReturned`] once its present in
    /// flight is answered: what a paint deferred behind it waits for. With
    /// none in flight it is owed at once.
    pub fn owe_frame(&mut self, window_id: u64) {
        let presenting = self.posted.iter().any(|posting| {
            posting.window_id == window_id && posting.kind == PostedRequest::Present
        });
        match self.record_mut(window_id) {
            Some(record) if presenting => record.frame_owed = true,
            Some(_) => self.land(Answer::FrameReturned { window_id }),
            None => {}
        }
    }

    /// Whether the session released window `window_id`'s pixels because
    /// nobody can see it, and has not asked for them again. Painting it now
    /// would map again the memory the release gave back; the
    /// [`WindowEvent::RedrawRequested`] that ends it comes before anyone can.
    #[must_use]
    pub fn is_hidden(&self, window_id: u64) -> bool {
        self.presented
            .iter()
            .any(|record| record.window_id == window_id && record.frames == Frames::Hidden)
    }

    /// Whether the session released its side of window `window_id`'s
    /// frames, so its next present must re-attach a region.
    #[cfg(any(test, all(freestanding, feature = "rt")))]
    pub(crate) fn content_released(&self, window_id: u64) -> bool {
        self.presented
            .iter()
            .any(|record| record.window_id == window_id && record.frames != Frames::Mapped)
    }

    /// Close window `window_id`, and with it every transient hung from it.
    ///
    /// The client forgets them at once — they are certainly not ones to
    /// keep re-presenting, nor to post to — and what they have in flight is
    /// still reaped, unreported, as is the close's own answer: a window the
    /// session no longer knows is closed already.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn close(&mut self, window_id: u64) -> Result<(), Errno> {
        // The session closes a window's transients with it.
        while let Some(transient) = self
            .presented
            .iter()
            .find(|record| record.owner == Some(window_id))
            .map(|record| record.window_id)
        {
            self.forget(transient);
        }
        self.forget(window_id);
        self.post_own(
            window_id,
            PostedRequest::Close,
            &WindowRequest::Close { window_id },
            Carried::Nothing,
        )
    }

    /// Re-map window `window_id` onto a fresh frame region: `frame_count`
    /// frames shaped as `surface`, laid out back-to-back in the region
    /// granted as `shm_handle`, keeping the same window id, title, and
    /// event endpoint.
    ///
    /// A resizable app calls this after the window manager tells it a new
    /// client size ([`WindowEvent::Resized`]): it renders into the new
    /// region and re-maps the existing window onto it, so the resize keeps
    /// the window identity rather than opening a new window. A refusal
    /// arrives later as an [`EventError::Refused`], followed by the size the
    /// session kept.
    ///
    /// # Errors
    ///
    /// [`Errno::WouldBlock`] while a resize of the window is unanswered or a
    /// request waits for room ahead of it — the window's newest size is
    /// delivered again once one is answered — or a transport failure.
    ///
    /// [`WindowEvent::Resized`]: tairix_abi::window_ipc::WindowEvent::Resized
    pub fn resize(
        &mut self,
        window_id: u64,
        shm_handle: u64,
        frame_count: u32,
        surface: &DisplayMode,
    ) -> Result<(), Errno> {
        if self.resize_in_flight(window_id) {
            return Err(Errno::WouldBlock);
        }
        self.posted.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
        let request = WindowRequest::Resize {
            window_id,
            shm_handle,
            frame_count,
            width_px: surface.width_px,
            height_px: surface.height_px,
            stride_bytes: surface.stride_bytes,
            format: surface.format,
        };
        let posted = if self.waits_for_room() {
            Err(Errno::WouldBlock)
        } else {
            self.post(&request)
        };
        let ticket = match posted {
            Ok(ticket) => ticket,
            Err(Errno::WouldBlock) => {
                // The size is delivered again once room is made.
                if let Some(record) = self.record_mut(window_id) {
                    record.deferred = true;
                }
                return Err(Errno::WouldBlock);
            }
            Err(err) => return Err(err),
        };
        self.posted.push(Posting {
            window_id,
            kind: PostedRequest::Resize,
            ticket: Some(ticket),
            held: None,
            carried: Carried::Nothing,
            orphaned: false,
        });
        // The old frames describe the old extent, so the remembered
        // present is stale until the app paints the new size.
        self.note_extent(window_id, surface.width_px, surface.height_px);
        Ok(())
    }

    /// Whether a resize of `window_id` is still unanswered once every answer
    /// that has landed is taken in. While one is, no other is sent: the
    /// window's newest reported geometry is delivered again as a
    /// [`WindowEvent::Resized`] once it is answered, so a caller asks this
    /// before making a region it could not yet hand over.
    pub fn resize_in_flight(&mut self, window_id: u64) -> bool {
        self.reap_posted();
        let in_flight = self
            .posted
            .iter()
            .any(|posting| posting.window_id == window_id && posting.kind == PostedRequest::Resize);
        if in_flight {
            if let Some(record) = self.record_mut(window_id) {
                record.deferred = true;
            }
        }
        in_flight
    }

    /// Retitle window `window_id` — one of the caller's own windows — to
    /// `title`, replacing the title given at creation.
    ///
    /// The session applies the new title to the window's chrome and to
    /// its taskbar entry together, so an app whose window shows a
    /// changing subject (the folder it is browsing, the document it
    /// holds) names that subject in both places from this one request.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] / [`Errno::OutOfRange`] — a title the
    /// protocol refuses (over-long, or holding a control character), caught
    /// before any request (never truncated) — or a transport failure.
    pub fn set_title(&mut self, window_id: u64, title: &str) -> Result<(), Errno> {
        let title = WindowTitle::new(title)?;
        self.post_or_hold(window_id, Asked::Title(title), Carried::Nothing)
    }

    /// Restate the range the window manager may resize window `window_id`
    /// within, replacing the range given at creation.
    ///
    /// An app whose content constraints move — a board switching to a larger
    /// one, a layout remeasured at a new desktop density — states the new
    /// range here. Without it the window manager holds a drag to the range
    /// of content the app is no longer showing. A window the new range no
    /// longer holds is brought inside it and answered with a
    /// [`WindowEvent::Resized`]. The session refuses a sizing that
    /// contradicts how the window was decorated [`Errno::NotSupported`]: what
    /// may be restated is the range, not whether the window is resizable.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] — a range naming no reachable size (a maximum
    /// below its own minimum), caught before any request — or a transport
    /// failure.
    pub fn set_sizing(&mut self, window_id: u64, sizing: WindowSizing) -> Result<(), Errno> {
        self.post_or_hold(window_id, Asked::Sizing(sizing), Carried::Nothing)
    }

    /// Ask for window `window_id` to be put into `state` — the app's half
    /// of exclusive fullscreen.
    ///
    /// The state the window manager actually applied arrives as a
    /// [`WindowEvent::Resized`] carrying it alongside the new client extent,
    /// so an app lays out from the event rather than from having asked.
    /// Leaving fullscreen means naming the state to return to. A window that
    /// cannot take the state — a fixed-size window has only the size it was
    /// created at — is refused [`Errno::NotSupported`].
    ///
    /// The window manager keeps the display path: it sizes the surface to
    /// the scan-out and withdraws the decoration, and the app presents
    /// exactly as it always did. There is no framebuffer to seize.
    ///
    /// # Errors
    ///
    /// A transport failure.
    ///
    /// [`WindowEvent::Resized`]: tairix_abi::window_ipc::WindowEvent::Resized
    pub fn set_size_state(&mut self, window_id: u64, state: WindowSizeState) -> Result<(), Errno> {
        self.post_or_hold(window_id, Asked::SizeState(state), Carried::Nothing)
    }

    /// Ask the session to run its trusted file picker for window
    /// `window_id` (`plans/CAPABILITY_USE.md` CU6).
    ///
    /// The pick concludes asynchronously with a [`WindowEvent::FilePicked`]
    /// (carrying the one-shot `fd_redeem` handle), a
    /// [`WindowEvent::FolderPicked`] or [`WindowEvent::SubtreePicked`] for the
    /// folder purposes, or a [`WindowEvent::PickCancelled`] on the app's event
    /// endpoint. The session refuses [`Errno::AlreadyExists`] while a pick is
    /// already pending on the window.
    ///
    /// # Errors
    ///
    /// A transport failure.
    ///
    /// [`WindowEvent::FilePicked`]: tairix_abi::window_ipc::WindowEvent::FilePicked
    /// [`WindowEvent::FolderPicked`]: tairix_abi::window_ipc::WindowEvent::FolderPicked
    /// [`WindowEvent::SubtreePicked`]: tairix_abi::window_ipc::WindowEvent::SubtreePicked
    /// [`WindowEvent::PickCancelled`]: tairix_abi::window_ipc::WindowEvent::PickCancelled
    pub fn pick_file(&mut self, window_id: u64, purpose: PickPurpose) -> Result<(), Errno> {
        self.post_own(
            window_id,
            PostedRequest::PickFile,
            &WindowRequest::PickFile { window_id, purpose },
            Carried::Nothing,
        )
    }

    /// Hand the session the drag the user began on `items` in window
    /// `window_id`, while the press that began it is still held. It is
    /// reported as it moves ([`WindowEvent::DragOver`]) and ends with one
    /// [`WindowEvent::DragEnded`]; a refusal to take the gesture arrives as
    /// an [`EventError::Refused`] instead.
    ///
    /// # Errors
    ///
    /// A transport failure.
    ///
    /// [`WindowEvent::DragOver`]: tairix_abi::window_ipc::WindowEvent::DragOver
    /// [`WindowEvent::DragEnded`]: tairix_abi::window_ipc::WindowEvent::DragEnded
    pub fn begin_drag(&mut self, window_id: u64, items: DragItems) -> Result<(), Errno> {
        self.post_own(
            window_id,
            PostedRequest::BeginDrag,
            &WindowRequest::BeginDrag { window_id, items },
            Carried::Nothing,
        )
    }

    /// Answer the drag report numbered `serial` for the drag window
    /// `window_id` began: what a drop there would do, or `None` to refuse it.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn drag_verdict(
        &mut self,
        window_id: u64,
        serial: u32,
        verdict: Option<DropOperation>,
    ) -> Result<(), Errno> {
        self.post_or_hold(
            window_id,
            Asked::DragVerdict(serial, verdict),
            Carried::Nothing,
        )
    }

    /// Answer the save offer numbered `serial` over window `window_id`:
    /// whether a drop there saves the document into a folder this
    /// application names.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn answer_save_offer(
        &mut self,
        window_id: u64,
        serial: u32,
        accepts: bool,
    ) -> Result<(), Errno> {
        self.post_or_hold(
            window_id,
            Asked::SaveOfferAnswer(serial, accepts),
            Carried::Nothing,
        )
    }

    /// Answer the desktop's items offer numbered `serial` over window
    /// `window_id` with what a drop there would do, or `None` to refuse it.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn answer_items_offer(
        &mut self,
        window_id: u64,
        serial: u32,
        verdict: Option<DropOperation>,
    ) -> Result<(), Errno> {
        self.post_or_hold(
            window_id,
            Asked::ItemsOfferAnswer(serial, verdict),
            Carried::Nothing,
        )
    }

    /// Answer the file offer numbered `serial` over window `window_id`:
    /// whether a drop there places the file in the window's document.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn answer_file_offer(
        &mut self,
        window_id: u64,
        serial: u32,
        places: bool,
    ) -> Result<(), Errno> {
        self.post_or_hold(
            window_id,
            Asked::FileOfferAnswer(serial, places),
            Carried::Nothing,
        )
    }

    /// Ask for the media type of the one file the [`WindowEvent::FileOffered`]
    /// numbered `serial` holds over window `window_id`, landing as
    /// [`Answer::OfferedFile`]. Asked again over the same window while one is
    /// unanswered, only the newest is sent.
    ///
    /// # Errors
    ///
    /// A transport failure.
    ///
    /// [`WindowEvent::FileOffered`]: tairix_abi::window_ipc::WindowEvent::FileOffered
    pub fn ask_offered_file(&mut self, window_id: u64, serial: u32) -> Result<(), Errno> {
        self.post_or_hold(window_id, Asked::OfferedFile(serial), Carried::Nothing)
    }

    /// Hand the file window `window_id`'s drag placed to the window it was
    /// dropped on, through `grant`, a delegation minted to the session from a
    /// descriptor opened read-only, called `name`. One-shot. The session
    /// refuses [`Errno::NotFound`] when the drag placed nothing or its drop
    /// was handed over already.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn place_dropped(
        &mut self,
        window_id: u64,
        name: DocumentName,
        grant: core::num::NonZeroU64,
    ) -> Result<(), Errno> {
        self.post_own(
            window_id,
            PostedRequest::PlaceDropped,
            &WindowRequest::PlaceDropped {
                window_id,
                name,
                grant,
            },
            Carried::Nothing,
        )
    }

    /// Ask for every item of the desktop's drag numbered `drag`, offered to
    /// or dropped on window `window_id`, landing in order as one
    /// [`Answer::OfferedItems`]: read a page a round trip, none of them
    /// waited on.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn ask_offered_items(&mut self, window_id: u64, drag: u32) -> Result<(), Errno> {
        self.post_or_hold(
            window_id,
            Asked::OfferedItems(drag, 0),
            Carried::Items(Vec::new()),
        )
    }

    /// Answer the save dropped on window `window_id`, numbered `serial`, with
    /// the folder it goes in: `location`, a `fd_grant_location` handle this
    /// process minted to the session from a folder it opened itself, or zero
    /// to decline. The session consumes the location whatever it answers.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn save_into(&mut self, window_id: u64, serial: u32, location: u64) -> Result<(), Errno> {
        self.post_own(
            window_id,
            PostedRequest::SaveInto(serial),
            &WindowRequest::SaveInto {
                window_id,
                serial,
                location,
            },
            Carried::Nothing,
        )
    }

    /// Tell the desktop that the drop numbered `serial` window `window_id`'s
    /// drag concluded there has ended: what a drop that ends before its own
    /// thread steps it owes. A refusal is not reported, since a drop the
    /// desktop already ended has nothing left to end.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn end_landing(&mut self, window_id: u64, serial: u32) -> Result<(), Errno> {
        self.post_own(
            window_id,
            PostedRequest::LandingEnded(serial),
            &WindowRequest::DropLanding {
                window_id,
                serial,
                step: LandingStep::Ended,
            },
            Carried::Nothing,
        )
    }

    /// Ask for the desktop folder the drag report numbered `serial` named, for
    /// the drag window `window_id` began, landing as [`Answer::DragSpot`].
    /// Asked again over the same window while one is unanswered, only the
    /// newest is sent.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn ask_drag_spot(&mut self, window_id: u64, serial: u32) -> Result<(), Errno> {
        self.post_or_hold(window_id, Asked::DragSpot(serial), Carried::Nothing)
    }

    /// Ask for the application window `window_id`'s drag was dropped on, once
    /// its [`WindowEvent::DragEnded`] said it was, landing as
    /// [`Answer::DropTarget`]. One-shot.
    ///
    /// # Errors
    ///
    /// A transport failure.
    ///
    /// [`WindowEvent::DragEnded`]: tairix_abi::window_ipc::WindowEvent::DragEnded
    pub fn ask_drop_target(&mut self, window_id: u64) -> Result<(), Errno> {
        self.post_own(
            window_id,
            PostedRequest::DropTarget,
            &WindowRequest::TakeDropTarget { window_id },
            Carried::Nothing,
        )
    }

    /// Ask for the name of the file or folder window `window_id`'s last pick
    /// chose, once its [`WindowEvent::FilePicked`] or
    /// [`WindowEvent::SubtreePicked`] has arrived, landing as
    /// [`Answer::PickedName`]. One-shot.
    ///
    /// # Errors
    ///
    /// A transport failure.
    ///
    /// [`WindowEvent::FilePicked`]: tairix_abi::window_ipc::WindowEvent::FilePicked
    /// [`WindowEvent::SubtreePicked`]: tairix_abi::window_ipc::WindowEvent::SubtreePicked
    pub fn ask_picked_name(&mut self, window_id: u64) -> Result<(), Errno> {
        self.post_own(
            window_id,
            PostedRequest::PickedName,
            &WindowRequest::TakePickedName { window_id },
            Carried::Nothing,
        )
    }

    /// Ask for the files window `window_id`'s last folder pick delegated, once
    /// its [`WindowEvent::FolderPicked`] has arrived: each lands as an
    /// [`Answer::PickedFile`], in order, and the last as
    /// [`Errno::NotFound`], once none is left.
    ///
    /// # Errors
    ///
    /// A transport failure.
    ///
    /// [`WindowEvent::FolderPicked`]: tairix_abi::window_ipc::WindowEvent::FolderPicked
    pub fn ask_picked_files(&mut self, window_id: u64) -> Result<(), Errno> {
        self.post_or_hold(window_id, Asked::PickedFile, Carried::Nothing)
    }

    /// One page of the shipped wallpaper catalog, from entry `from`.
    ///
    /// The session lists the read-only shipped store once and answers from
    /// what it holds, so this costs no I/O either side. `into` is the
    /// caller's own page buffer, held once rather than taken per call, and
    /// the answer borrows from it: the catalog's total length and the
    /// entries this page carried. A caller with more entries than one page
    /// holds asks again from where the page ended.
    ///
    /// A call, which waits on the session's loop: made where no frame is owed
    /// — at start-up, or on a worker — never on an interactive loop.
    ///
    /// # Errors
    ///
    /// The session's typed refusal ([`Errno::NotSupported`] from a session
    /// that serves no catalog), a transport failure, or a malformed reply
    /// (fail closed, never a guessed catalog).
    pub fn wallpapers<'a>(
        &mut self,
        from: u16,
        into: &'a mut [u8; WINDOW_WALLPAPERS_REPLY_MAX],
    ) -> Result<WallpaperPage<'a>, Errno> {
        let n = self.call(&WindowRequest::QueryWallpapers { from }, into)?;
        let frame = into.get(..n).ok_or(Errno::LengthOutOfRange)?;
        decode_wallpapers_reply(frame)
    }

    /// Entry `index` of the screensaver catalogue, with the catalogue's total,
    /// for the desktop's own Settings application: the desktop's own two, then
    /// every saver bundle installed for the system and for the user, each with
    /// whether it can be chosen. `into` is the caller's own reply buffer, and
    /// the answer borrows from it. A call, made only where no frame is owed.
    ///
    /// # Errors
    ///
    /// [`Errno::PermissionDenied`] for any other caller, [`Errno::NotFound`]
    /// past the last entry, a transport failure, or a malformed reply.
    pub fn screensaver<'a>(
        &mut self,
        index: u16,
        into: &'a mut [u8; WINDOW_SCREENSAVER_REPLY_MAX],
    ) -> Result<(u16, ScreensaverEntry<'a>), Errno> {
        let n = self.call(&WindowRequest::QueryScreensaver { index }, into)?;
        let frame = into.get(..n).ok_or(Errno::LengthOutOfRange)?;
        decode_screensaver_reply(frame)
    }

    /// Every cursor set this desktop offers, in the order a chooser lists
    /// them.
    ///
    /// The session lists the read-only shipped store once, at bring-up, and
    /// answers from what it holds, so this costs no I/O either side. `into`
    /// is the caller's own reply buffer, held once rather than taken per
    /// call, and the answer borrows from it. There is no paging: the whole
    /// choice space fits one reply by construction. A call, made only where
    /// no frame is owed.
    ///
    /// # Errors
    ///
    /// The session's typed refusal ([`Errno::NotSupported`] from a session
    /// that offers no sets of its own), a transport failure, or a malformed
    /// reply (fail closed, never a guessed choice space).
    pub fn cursor_sets<'a>(
        &mut self,
        into: &'a mut [u8; WINDOW_CURSOR_SETS_REPLY_MAX],
    ) -> Result<NameList<'a>, Errno> {
        let n = self.call(&WindowRequest::QueryCursorSets, into)?;
        let frame = into.get(..n).ok_or(Errno::LengthOutOfRange)?;
        decode_cursor_sets_reply(frame)
    }

    /// The bundle identities of every source that has posted a notification
    /// since the desktop started, in the order they first did.
    ///
    /// The session answers only its own Settings application. `into` is the
    /// caller's own reply buffer and the answer borrows from it. A call, made
    /// only where no frame is owed.
    ///
    /// # Errors
    ///
    /// The session's refusal ([`Errno::PermissionDenied`] for any other
    /// caller), a transport failure, or a malformed reply.
    pub fn notify_sources<'a>(
        &mut self,
        into: &'a mut [u8; WINDOW_NOTIFY_SOURCES_REPLY_MAX],
    ) -> Result<NameList<'a>, Errno> {
        let n = self.call(&WindowRequest::QueryNotifySources, into)?;
        let frame = into.get(..n).ok_or(Errno::LengthOutOfRange)?;
        decode_notify_sources_reply(frame)
    }

    /// Ask the session to lock the screen now. A call, made only where no
    /// frame is owed.
    ///
    /// # Errors
    ///
    /// The session's refusal ([`Errno::PermissionDenied`] for any caller but
    /// its own Settings application) or a transport failure.
    pub fn lock_screen(&mut self) -> Result<(), Errno> {
        self.status_call(&WindowRequest::LockScreen)
    }

    /// Ask the desktop to authorise and perform `task` for window
    /// `window_id`, or for this application's icon-bar slot when `None`. The
    /// outcome arrives as a [`WindowEvent::Authorised`]; a captured task's
    /// output is then taken with
    /// [`take_authorised_output`](Self::take_authorised_output).
    ///
    /// The session's refusal arrives as an [`EventError::Refused`]:
    /// [`Errno::PermissionDenied`] for any caller but the component the task
    /// belongs to, [`Errno::OutOfRange`] for a task it will not perform as
    /// asked, [`Errno::AlreadyExists`] while one is pending on the window or
    /// slot, [`Errno::NotFound`] for a slot never declared, and
    /// [`Errno::SeatBusy`] while anything else holds the seat.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for window `0`, which names none, or a task the
    /// wire cannot carry, or a transport failure.
    pub fn authorise(&mut self, window_id: Option<u64>, task: &SystemTask) -> Result<(), Errno> {
        let request = WindowRequest::Authorise {
            window_id: asked_window(window_id)?,
            task: *task,
        };
        self.post_own(
            window_id.unwrap_or(0),
            PostedRequest::Authorise,
            &request,
            Carried::Nothing,
        )
    }

    /// What the last task asked for window `window_id`, or for this
    /// application's icon-bar slot when `None`, printed, once its
    /// [`WindowEvent::Authorised`] said it was captured. One-shot. A call,
    /// made only where no frame is owed.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for window `0`, which names none,
    /// [`Errno::NotFound`] when there is none to take, a transport failure,
    /// or a corrupt reply.
    pub fn take_authorised_output(&mut self, window_id: Option<u64>) -> Result<Vec<u8>, Errno> {
        let window_id = asked_window(window_id)?;
        let output = decode_authorised_output_reply(
            self.pull(&WindowRequest::TakeAuthorisedOutput { window_id })?,
        )?;
        let mut taken = Vec::new();
        taken
            .try_reserve_exact(output.len())
            .map_err(|_| Errno::OutOfMemory)?;
        taken.extend_from_slice(output);
        Ok(taken)
    }

    /// A read-only delegation of the desktop's civil-time store: the
    /// `fd_grant` handle the session minted to this process, redeemed from
    /// [`session`](Self::session). A call, made only where no frame is owed.
    ///
    /// # Errors
    ///
    /// The session's refusal ([`Errno::PermissionDenied`] for any caller but
    /// its own Settings application, [`Errno::NotFound`] when the desktop
    /// holds no store), a transport failure, or a malformed reply.
    pub fn zone_store(&mut self) -> Result<u64, Errno> {
        decode_handle_reply(self.pull(&WindowRequest::TakeZoneStore)?)
    }

    /// Ask the session to show the screensaver `document` describes, now, as
    /// a preview. A call, made only where no frame is owed.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] or [`Errno::OutOfRange`] for a document
    /// the wire cannot carry, the session's refusal
    /// ([`Errno::PermissionDenied`] for any caller but its own Settings
    /// application, [`Errno::OutOfRange`] for a document it will not read,
    /// [`Errno::SeatBusy`] while a lock or the trusted picker holds the
    /// seat), or a transport failure.
    pub fn preview_screensaver(&mut self, document: &str) -> Result<(), Errno> {
        self.status_call(&WindowRequest::PreviewScreensaver {
            document: tairix_abi::pinboard_ipc::PinboardDocument::new(document)?,
        })
    }

    /// Ask the session to render `subject` as a `width`x`height` picture into
    /// the region granted as `shm_handle`, concluding to window `window_id`.
    ///
    /// A success is only the acceptance: the session decodes the untrusted
    /// picture in its own parser sandbox, off its compositing loop, and
    /// concludes with a [`WindowEvent::PreviewRendered`] on the app's event
    /// endpoint. A call, made only where no frame is owed.
    ///
    /// # Errors
    ///
    /// The session's typed refusal ([`Errno::LimitExceeded`] while the app
    /// has as many renders pending as the desktop runs at once, or one of the
    /// window's conclusions waits undelivered in a full mailbox, and
    /// [`Errno::AlreadyExists`] for a picture already pending at that size —
    /// both answered by asking again once one concludes; [`Errno::NotFound`]
    /// for a window the caller does not own, a subject the desktop does not
    /// hold, or a region not granted to it; [`Errno::LengthOutOfRange`] for a
    /// region too small for the size;
    /// [`Errno::Busy`] while the desktop is shutting down;
    /// [`Errno::OutOfMemory`] when it cannot queue the render), a transport
    /// failure, or a corrupt status frame.
    ///
    /// [`WindowEvent::PreviewRendered`]: tairix_abi::window_ipc::WindowEvent::PreviewRendered
    pub fn render_preview(
        &mut self,
        (window_id, shm_handle): (u64, u64),
        subject: PreviewSubject,
        (width, height): (u16, u16),
    ) -> Result<(), Errno> {
        self.status_call(&WindowRequest::RenderPreview {
            window_id,
            shm_handle,
            subject,
            width,
            height,
        })
    }

    /// Ask the desktop to open `menu` as a menu chain for this app's window
    /// `window_id`, anchored at `anchor` in that window's own client pixels
    /// (`plans/NEW-MENUS.md`).
    ///
    /// The app describes and the desktop decides: it supplies the rows, the
    /// root plate's title ([`AppMenu::titled`]) and the anchor, and the
    /// session titles, places, draws, grabs, routes, dismisses — and
    /// answers. Returns the **open id**, named by this client: the whole
    /// answer arrives as exactly one [`WindowEvent::MenuClosed`] naming it,
    /// or as an [`EventError::Refused`] naming it when the session will not
    /// open the menu — [`Errno::NotSupported`] from a desktop that composes no
    /// menu service, over which the caller carries on and never draws a menu
    /// of its own, and [`Errno::AlreadyExists`] while an open on the window is
    /// unanswered.
    ///
    /// Match the open id rather than assuming the next outcome is this
    /// open's: an answer to a previous gesture may still be in the app's
    /// mailbox when this one is asked.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for an empty menu, which would open nothing;
    /// [`Self::create`]'s naming errors; or a transport failure.
    ///
    /// [`WindowEvent::MenuClosed`]: tairix_abi::window_ipc::WindowEvent::MenuClosed
    pub fn open_menu(
        &mut self,
        window_id: u64,
        anchor: WindowRegion,
        menu: &AppMenu,
    ) -> Result<u64, Errno> {
        let open_id = self.mint()?;
        let request = WindowRequest::OpenMenu {
            window_id,
            open_id,
            anchor,
            menu: *menu,
        };
        self.post_own(
            window_id,
            PostedRequest::OpenMenu(open_id),
            &request,
            Carried::Nothing,
        )?;
        Ok(open_id)
    }

    /// Show `menu` in the chain opened as `open_id` on `window_id` from now
    /// on: what its rows say once a [`WindowEvent::MenuChosen`] has changed
    /// it, or once anything else has. The chain keeps its place, its open
    /// plates and its highlight wherever the rows still have them, and every
    /// row chosen from `menu` answers with `revision`, so one chosen from the
    /// rows before it is told apart.
    ///
    /// A restate that crosses the chain's answer in flight changes nothing
    /// and is not refused; one naming another window's open, or an open
    /// never made, is. One asked while another of the same open is
    /// unanswered is held in its place, so a burst reaches the session as
    /// its first and its newest.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] for an empty menu, or a transport failure.
    ///
    /// [`WindowEvent::MenuChosen`]: tairix_abi::window_ipc::WindowEvent::MenuChosen
    pub fn restate_menu(
        &mut self,
        window_id: u64,
        open_id: u64,
        revision: u32,
        menu: &AppMenu,
    ) -> Result<(), Errno> {
        let request = WindowRequest::RestateMenu {
            window_id,
            open_id,
            revision,
            menu: *menu,
        };
        self.post_own(
            window_id,
            PostedRequest::RestateMenu(open_id),
            &request,
            Carried::Nothing,
        )
    }

    /// Put `alert` to the user in a window of its own, holding `owner` — one
    /// of this app's own top-level windows — until it is answered, or no
    /// window when `None`. Returns the alert's id, named by this client, which
    /// its one [`WindowEvent::AlertClosed`] names, delivered to
    /// `event_endpoint`.
    ///
    /// The session's refusal arrives as an [`EventError::Refused`] naming the
    /// alert: [`Errno::NotSupported`] from a desktop that draws no alerts,
    /// over which the caller states its message on `stderr` instead and
    /// carries on; [`Errno::LimitExceeded`] for an app that already has
    /// [`ALERT_MAX_OPEN`](tairix_abi::window_ipc::ALERT_MAX_OPEN) up;
    /// [`Errno::NotFound`] for an `owner` that is not one of this app's
    /// windows; [`Errno::OutOfRange`] for one that is a popup, tool or layer
    /// surface.
    ///
    /// # Errors
    ///
    /// [`Self::create`]'s naming errors, or a transport failure.
    ///
    /// [`WindowEvent::AlertClosed`]: tairix_abi::window_ipc::WindowEvent::AlertClosed
    pub fn open_alert(
        &mut self,
        event_endpoint: u64,
        owner: Option<u64>,
        alert: &AlertSpec,
    ) -> Result<u64, Errno> {
        let alert_id = self.mint()?;
        let request = WindowRequest::OpenAlert {
            alert_id,
            event_endpoint,
            owner_window_id: owner.unwrap_or(0),
            alert: *alert,
        };
        self.post_own(
            owner.unwrap_or(0),
            PostedRequest::OpenAlert(alert_id),
            &request,
            Carried::Nothing,
        )?;
        Ok(alert_id)
    }

    /// Withdraw this app's alert `alert_id`; it is answered `Dismissed`. A
    /// refusal is not reported: an alert already answered has nothing left
    /// to withdraw.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn close_alert(&mut self, alert_id: u64) -> Result<(), Errno> {
        self.post_own(
            0,
            PostedRequest::CloseAlert(alert_id),
            &WindowRequest::CloseAlert { alert_id },
            Carried::Nothing,
        )
    }

    /// Ask for every target queued for this application to open: each lands
    /// as an [`Answer::OpenTarget`], in order, and the last as `None`, once
    /// the queue is drained.
    ///
    /// The answer to a [`WindowEvent::OpenRequested`] wake, which says only
    /// that *at least one* target is waiting. Asked again while the queue is
    /// being read, it is read once more after, so a target queued meanwhile
    /// is never left behind.
    ///
    /// A [`Target::Path`] is the file or folder the user asked this
    /// application to open, and confers no access — the application opens it
    /// under its own authority, exactly as it would a path in its own
    /// argument list. A [`Target::Document`] is a file *already* opened by
    /// whoever handed it over, reachable through a one-shot delegation the
    /// kernel minted to this application, which is the only form an
    /// application holding no filesystem capability can act on. The session
    /// answers [`Errno::NotFound`] for an application with no target queue
    /// and [`Errno::NotSupported`] for a session serving none.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn ask_open_targets(&mut self) -> Result<(), Errno> {
        self.post_or_hold(0, Asked::OpenTarget, Carried::Nothing)
    }

    /// Ask for the text the user committed into the quick-entry field of the
    /// chain this window opened as `open_id`, landing as
    /// [`Answer::MenuText`]: `None` when the session holds none for it.
    ///
    /// The answer to a [`MenuOutcome::Entered`](tairix_abi::window_ipc::MenuOutcome::Entered):
    /// that outcome names *which field* was committed, and the text is asked
    /// for because an event is one fixed frame and a name is wider than that.
    ///
    /// **Taken once.** A second ask for the same commit lands `None`, and so
    /// does one naming an open the session is no longer holding a text for —
    /// the next open on this window clears it — so a stale ask can never
    /// return a name the user typed into an earlier gesture.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn ask_menu_text(&mut self, window_id: u64, open_id: u64) -> Result<(), Errno> {
        self.post_own(
            window_id,
            PostedRequest::MenuText(open_id),
            &WindowRequest::TakeMenuText { window_id, open_id },
            Carried::Nothing,
        )
    }

    /// Ask for the text the user committed into this application's icon-bar
    /// row `item` on the menu of its declaration `revision`, answering a
    /// [`WindowEvent::AppBarEntered`] with the row and revision it carried:
    /// it lands as [`Answer::AppBarText`], `None` where the row holds none
    /// under that revision, or it was already taken.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn ask_app_bar_text(&mut self, item: AppMenuItemId, revision: u32) -> Result<(), Errno> {
        self.post_own(
            0,
            PostedRequest::AppBarText(item, revision),
            &WindowRequest::TakeAppBarText { item, revision },
            Carried::Nothing,
        )
    }

    /// Ask the session to reach the live instance of the bundle whose entry
    /// binary is `run_path`, handing it `target`, answering the token its
    /// [`Answer::HandOver`] names.
    ///
    /// The single-instance funnel for a launcher that is not the desktop: a
    /// file manager that spawns a viewer per document otherwise bypasses it
    /// and a bundle declaring one instance gets several. A document's grant
    /// is minted by the *caller*, to the session
    /// ([`session`](Self::session)), from a descriptor the caller opened
    /// itself — the session opens nothing on the caller's behalf.
    ///
    /// [`HandOverOutcome::NotRunning`] is an answer rather than a refusal: it
    /// says there was no instance to reach, so the caller launches the bundle
    /// itself.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] — `run_path` is longer than a hand-over
    /// may name; the caller launches the bundle the ordinary way — or a
    /// transport failure.
    pub fn hand_over_launch(
        &mut self,
        run_path: &str,
        target: HandOverTarget,
    ) -> Result<u64, Errno> {
        let request = WindowRequest::HandOverLaunch {
            run_path: BundleRunPath::new(run_path)?,
            target,
        };
        let token = self.next_token;
        self.post_own(
            0,
            PostedRequest::HandOver(token),
            &request,
            Carried::Nothing,
        )?;
        self.next_token = token.wrapping_add(1).max(1);
        Ok(token)
    }

    /// Ask the desktop to show the user, in the file manager, the document
    /// behind this client's window `window_id`, which `location` — a
    /// `fd_grant_location` handle minted to the [`session`](Self::session)
    /// from the document's descriptor — tells the desktop the place of.
    ///
    /// The caller never learns the path. Answers the token its
    /// [`Answer::Revealed`] names, which lands whether or not the window has
    /// closed since, since a document is shown as its window goes:
    /// [`Errno::NotFound`] when no file manager is running to show it,
    /// [`Errno::PermissionDenied`] when the user is not working in the window
    /// or the desktop may not be told where the document is, and
    /// [`Errno::NotSupported`] from a desktop that shows no documents.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn reveal_document(&mut self, window_id: u64, location: u64) -> Result<u64, Errno> {
        let token = self.next_token;
        self.post_own(
            0,
            PostedRequest::RevealDocument(token),
            &WindowRequest::RevealDocument {
                window_id,
                location,
            },
            Carried::Nothing,
        )?;
        self.next_token = token.wrapping_add(1).max(1);
        Ok(token)
    }

    /// Declare — or withdraw — the tooltip for `region` of `window_id`.
    ///
    /// The application says only what is being explained and where; the dwell
    /// before the tip appears, where its plate goes, what it is drawn with,
    /// and every reason it comes down are the desktop's. A window holds at
    /// most one declaration, so this replaces any previous one, and empty
    /// `text` withdraws it — taking a tip that is on screen down with it. A
    /// session that shows no tooltips refuses [`Errno::NotSupported`]; the
    /// tip is incidental, so the caller reports it and carries on.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn set_tooltip(
        &mut self,
        window_id: u64,
        region: WindowRegion,
        text: TooltipText,
    ) -> Result<(), Errno> {
        self.post_or_hold(window_id, Asked::Tooltip(region, text), Carried::Nothing)
    }

    /// Show `shape` for the pointer over the client area of this client's
    /// own window `window_id`, until asked for another.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn set_cursor(&mut self, window_id: u64, shape: CursorShape) -> Result<(), Errno> {
        self.post_or_hold(window_id, Asked::Cursor(shape), Carried::Nothing)
    }

    /// Read the keyboard as `mode` while this client's own window `window_id`
    /// holds it, until asked for another.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn set_keyboard_mode(&mut self, window_id: u64, mode: KeyboardMode) -> Result<(), Errno> {
        self.post_or_hold(window_id, Asked::KeyboardMode(mode), Carried::Nothing)
    }

    /// Lock the pointer to this client's own window `window_id` (`locked`),
    /// or let it go. A granted lock, and every end of one, arrives as
    /// [`WindowEvent::PointerLock`]; a refusal as the request's.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn lock_pointer(&mut self, window_id: u64, locked: bool) -> Result<(), Errno> {
        self.post_or_hold(window_id, Asked::PointerLock(locked), Carried::Nothing)
    }

    /// Raise this client's own window `window_id` and give it the keyboard,
    /// under an activation the session handed this client or while one of its
    /// windows already holds the keyboard; refused [`Errno::PermissionDenied`]
    /// with neither.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn activate_window(&mut self, window_id: u64) -> Result<(), Errno> {
        self.post_own(
            window_id,
            PostedRequest::Activate,
            &WindowRequest::ActivateWindow { window_id },
            Carried::Nothing,
        )
    }

    /// Put `bytes` on the clipboard as `kind`, from this client's own focused
    /// window `window_id`, through a region granted to the session and kept
    /// mapped until it has answered. The session refuses
    /// [`Errno::PermissionDenied`] for a window without the keyboard.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] past [`CLIPBOARD_MAX_BYTES`];
    /// [`Errno::OutOfMemory`] when the region cannot be made; or a transport
    /// failure.
    pub fn set_clipboard(
        &mut self,
        window_id: u64,
        kind: ClipboardKind,
        bytes: &[u8],
    ) -> Result<(), Errno> {
        if bytes.len() > CLIPBOARD_MAX_BYTES {
            return Err(Errno::LengthOutOfRange);
        }
        let (mut region, shm_handle) = self.transport.grant_region(bytes.len().max(1))?;
        region
            .bytes_mut()
            .get_mut(..bytes.len())
            .ok_or(Errno::OutOfMemory)?
            .copy_from_slice(bytes);
        let len = u64::try_from(bytes.len()).map_err(|_| Errno::LengthOutOfRange)?;
        self.post_own(
            window_id,
            PostedRequest::SetClipboard,
            &WindowRequest::SetClipboard {
                window_id,
                shm_handle,
                len,
                kind,
            },
            Carried::Region(region, false),
        )
    }

    /// Ask for what the clipboard holds, for this client's own focused window
    /// `window_id`, landing as [`Answer::Clipboard`]: offered through a region
    /// most payloads fit, and a longer one through a second region of the
    /// length the session stated.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfMemory`] when the region cannot be made, or a transport
    /// failure.
    pub fn ask_clipboard(&mut self, window_id: u64) -> Result<(), Errno> {
        let (region, shm_handle) = self.transport.grant_region(clipboard::FIRST_OFFER)?;
        self.post_or_hold(
            window_id,
            Asked::Clipboard(shm_handle),
            Carried::Region(region, false),
        )
    }

    /// Declare this **application's** presence on the desktop's icon bar:
    /// where its bar events arrive, whether it handles the primary click
    /// itself, and the menu a secondary press opens.
    ///
    /// Scoped to the process, not to a window, so an application keeps its
    /// slot with no window open — which is what makes a "new window"
    /// primary click and a *Quit* menu row reachable. Declaring it again
    /// replaces the declaration whole, which is how a row's enablement or
    /// mark changes; declared again while one is unanswered, only the newest
    /// is sent. A session that composes no icon bar refuses
    /// [`Errno::NotSupported`]; the declaration is incidental, so the caller
    /// reports it and carries on.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] — a reserved event endpoint, caught by the
    /// protocol before any request — or a transport failure.
    pub fn set_app_bar(&mut self, bar: &AppBar) -> Result<(), Errno> {
        self.post_own(
            0,
            PostedRequest::AppBar,
            &WindowRequest::SetAppBar(*bar),
            Carried::Nothing,
        )
    }

    /// Set window `window_id`'s backdrop-blur radius to `radius_px`
    /// logical pixels: the session blurs whatever is already composited
    /// behind the window's rectangle before blending the window's own
    /// (typically translucent) pixels over it, so a frosted-glass panel
    /// reads correctly. `0` disables the effect.
    ///
    /// The radius is a request the compositor honours as its own retention
    /// budget allows: a window buried under a pile of frosted ones may be
    /// composited with its opacity alone. That is not an error and is not
    /// reported as one.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] for a radius above
    /// [`tairix_abi::window_ipc::WINDOW_BACKDROP_BLUR_MAX_PX`], caught before
    /// any request, or a transport failure.
    pub fn set_backdrop_blur(&mut self, window_id: u64, radius_px: u16) -> Result<(), Errno> {
        self.post_or_hold(window_id, Asked::BackdropBlur(radius_px), Carried::Nothing)
    }

    /// Re-present window `window_id`'s last presented frame with
    /// full-window damage, answering a session redraw request.
    ///
    /// Returns whether a frame was re-presented: a window this client
    /// does not own, one that has not presented since it was created
    /// or resized, or one whose frames the session released has nothing to
    /// re-send and is ignored. A present still in flight is followed, not
    /// waited for: this runs inside the application's event drain.
    ///
    /// # Errors
    ///
    /// A transport failure.
    pub fn answer_redraw(&mut self, window_id: u64) -> Result<bool, Errno> {
        let Some(record) = self
            .presented
            .iter()
            .copied()
            .find(|record| record.window_id == window_id)
        else {
            return Ok(false);
        };
        // A released window's frames are gone on the session's side, so only
        // the application's own next paint can bring it back.
        let (Frames::Mapped, Some(frame_index)) = (record.frames, record.frame_index) else {
            return Ok(false);
        };
        let damage = whole(record.width_px, record.height_px)?;
        self.post_or_hold(
            window_id,
            Asked::Present(frame_index, damage),
            Carried::Nothing,
        )?;
        Ok(true)
    }

    /// The oldest answer landed and not yet taken. Answers land as the event
    /// drain takes in what the session sent back, and a
    /// [`WindowEvents::wait`] returns once one has.
    pub fn take_answer(&mut self) -> Option<Answer> {
        self.answers.pop_front()
    }

    /// Wait until nothing posted is unanswered, answering the oldest refusal
    /// among what was: what a thread that owes no frame — a worker carrying
    /// requests for an interactive loop — makes of a posted request a call.
    ///
    /// # Errors
    ///
    /// The oldest refusal, or why the wait ended with a request unanswered.
    pub fn await_answers(&mut self) -> Result<(), Errno> {
        self.send_waiting_as_calls();
        while let Some(index) = self
            .posted
            .iter()
            .position(|posting| posting.ticket.is_some())
        {
            let Some(ticket) = self.posted[index].ticket else {
                break;
            };
            let Self {
                transport,
                reply,
                posted,
                lost,
                ..
            } = self;
            let answer = match transport.await_reply(ticket, reply) {
                Ok(len) => reply.get(..len).ok_or(Errno::BufferTooSmall),
                // A call still live when its await ends is a reaper's again;
                // forgotten, its answer would keep the reply member ready for
                // ever.
                Err(errno) => match transport.reap(ticket, reply) {
                    Ok(None) => return Err(errno),
                    Ok(Some(len)) => reply.get(..len).ok_or(Errno::BufferTooSmall),
                    Err(over) => {
                        *lost |= over == Errno::BrokenPipe;
                        Err(over)
                    }
                },
            };
            let outcome = read_answer(&mut posted[index], answer, transport);
            self.answered(index, outcome);
            self.send_waiting_as_calls();
        }
        self.refused
            .pop_front()
            .map_or(Ok(()), |refused| Err(refused.errno))
    }

    /// Remember `window_id`'s client extent, discarding any frame index
    /// recorded against a previous extent and any present not yet sent, which
    /// names a frame of the region the extent replaced.
    fn note_extent(&mut self, window_id: u64, width_px: u32, height_px: u32) {
        let Some(record) = self.record_mut(window_id) else {
            return;
        };
        record.width_px = width_px;
        record.height_px = height_px;
        record.frame_index = None;
        // A fresh region, which the application paints whole; a hidden
        // window stays hidden until the session asks for it again.
        if record.frames == Frames::Released {
            record.frames = Frames::Mapped;
        }
        record.owed_whole = false;
        self.forget_unsent(window_id, |kind| kind == PostedRequest::Present);
    }

    /// Forget window `window_id`: its record, what it asked and has not sent,
    /// and what landed for it. What it has in flight is still reaped,
    /// unreported.
    fn forget(&mut self, window_id: u64) {
        self.presented
            .retain(|record| record.window_id != window_id);
        self.forget_unsent(window_id, |_| true);
        for posting in &mut self.posted {
            if posting.window_id == window_id {
                posting.orphaned = true;
            }
        }
        self.answers
            .retain(|answer| answer.window_id() != Some(window_id));
    }

    /// Forget what window `window_id` asked of each kind `forgets` names and
    /// has not sent: what waits for room, and what is held behind its kind's
    /// answer. What is in flight is still reaped.
    fn forget_unsent(&mut self, window_id: u64, forgets: fn(PostedRequest) -> bool) {
        self.posted.retain(|posting| {
            posting.window_id != window_id || posting.ticket.is_some() || !forgets(posting.kind)
        });
        for posting in &mut self.posted {
            if posting.window_id == window_id && forgets(posting.kind) {
                posting.held = None;
            }
        }
    }

    /// Note the geometry the session reported for a window in `event`.
    fn note_reported(&mut self, event: &WindowEvent) {
        if let WindowEvent::Resized {
            window_id,
            width_px,
            height_px,
            state,
        } = *event
        {
            if let Some(record) = self.record_mut(window_id) {
                record.reported = Some((width_px, height_px, state));
            }
        }
    }

    /// Whether a request waits for room, so one asked now waits behind it.
    fn waits_for_room(&self) -> bool {
        self.posted.iter().any(|posting| posting.ticket.is_none())
    }

    /// Post `asked` for `window_id`, keeping `carried` until it is answered —
    /// or hold it: where its kind supersedes, behind an unanswered one of its
    /// kind as the newest wanted; otherwise behind whatever waits for room,
    /// so requests reach the session in the order asked.
    fn post_or_hold(
        &mut self,
        window_id: u64,
        asked: Asked,
        carried: Carried<T::Region>,
    ) -> Result<(), Errno> {
        let kind = asked.kind();
        // Encoded first, so what the wire cannot carry is refused now rather
        // than when a held request is posted.
        let len = asked.request(window_id).encode(&mut self.frame)?;
        if kind.supersedes() {
            let extent = self
                .record_mut(window_id)
                .map(|record| (record.width_px, record.height_px));
            if let Some(posting) = self
                .posted
                .iter_mut()
                .find(|posting| posting.window_id == window_id && posting.kind.folds(kind))
            {
                let newest = match (posting.held.take(), asked) {
                    (Some(Held::Asked(Asked::Present(frame, damage))), Asked::Present(..)) => {
                        merged_present((frame, damage), asked, extent)
                    }
                    _ => asked,
                };
                posting.held = Some(Held::Asked(newest));
                return Ok(());
            }
        }
        self.posted.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
        let ticket = self.post_frame(len)?;
        self.posted.push(Posting {
            window_id,
            kind,
            ticket,
            held: ticket.is_none().then_some(Held::Asked(asked)),
            carried,
            orphaned: false,
        });
        Ok(())
    }

    /// Post `request`, of `kind`, for `window_id`, keeping `carried` until it
    /// is answered: as its own, or — where its kind supersedes — held behind
    /// an unanswered one of its kind as the newest wanted. One that waits for
    /// room keeps its encoded frame.
    fn post_own(
        &mut self,
        window_id: u64,
        kind: PostedRequest,
        request: &WindowRequest,
        carried: Carried<T::Region>,
    ) -> Result<(), Errno> {
        let len = request.encode(&mut self.frame)?;
        let index = kind
            .supersedes()
            .then(|| {
                self.posted
                    .iter()
                    .position(|posting| posting.window_id == window_id && posting.kind.folds(kind))
            })
            .flatten();
        if let Some(index) = index {
            self.posted[index].held = Some(Held::Frame(kept(&self.frame[..len])?));
            return Ok(());
        }
        self.posted.try_reserve(1).map_err(|_| Errno::OutOfMemory)?;
        let ticket = self.post_frame(len)?;
        let held = match ticket {
            Some(_) => None,
            None => Some(Held::Frame(kept(&self.frame[..len])?)),
        };
        self.posted.push(Posting {
            window_id,
            kind,
            ticket,
            held,
            carried,
            orphaned: false,
        });
        Ok(())
    }

    /// Post the request encoded in the first `len` bytes of the frame
    /// scratch, answering its ticket, or `None` when it must wait for room:
    /// behind a request already waiting, or for this process's place at the
    /// endpoint.
    fn post_frame(&mut self, len: usize) -> Result<Option<u64>, Errno> {
        if self.waits_for_room() {
            return Ok(None);
        }
        let frame = self.frame.get(..len).ok_or(Errno::BufferTooSmall)?;
        match self.transport.post(frame) {
            Ok(ticket) => Ok(Some(ticket)),
            Err(Errno::WouldBlock) => Ok(None),
            Err(errno) => Err(errno),
        }
    }

    /// Take in every posted request's answer that has landed, never waiting:
    /// a refusal is kept for the application, data is landed for it, a resize
    /// settles its window's geometry, and whatever goes in an answered one's
    /// place is posted. The requests waiting for room are then posted, oldest
    /// first — any call of this process can free room, not only one reaped
    /// here — and a resize that waited for room has its size delivered again.
    pub fn reap_posted(&mut self) {
        let mut answered = false;
        let mut index = 0;
        while let Some(&Posting { ticket: maybe, .. }) = self.posted.get(index) {
            let Some(ticket) = maybe else {
                index += 1;
                continue;
            };
            let Self {
                transport,
                reply,
                posted,
                lost,
                ..
            } = self;
            let answer = match transport.reap(ticket, reply) {
                Ok(None) => {
                    index += 1;
                    continue;
                }
                Ok(Some(len)) => reply.get(..len).ok_or(Errno::BufferTooSmall),
                Err(err) => {
                    *lost |= err == Errno::BrokenPipe;
                    Err(err)
                }
            };
            let outcome = read_answer(&mut posted[index], answer, transport);
            answered = true;
            if self.answered(index, outcome) {
                index += 1;
            }
        }
        if self.post_waiting() || answered {
            self.redeliver_deferred();
        }
    }

    /// Act on `outcome`, the answer to the posting at `index`, and post what
    /// goes in its place — the request reading on from it, else the newest of
    /// its kind held behind it — answering whether a posting stays at `index`.
    fn answered(&mut self, index: usize, outcome: Outcome) -> bool {
        if index >= self.posted.len() {
            return false;
        }
        let mut posting = self.posted.remove(index);
        let next = match outcome {
            Outcome::Status(status) => {
                self.settle_status(&posting, status);
                None
            }
            Outcome::Read(landed, next) => {
                if let Some(answer) = landed.filter(|_| !posting.orphaned) {
                    self.land(answer);
                }
                next.map(|next| Held::Asked(next.asked()))
            }
        };
        // What reads on from the answer stands for its held twin.
        let held = posting.held.take();
        let Some(follow) = next.or(held).filter(|_| !posting.orphaned) else {
            self.retire(&posting);
            return false;
        };
        posting.kind = follow.kind(posting.kind);
        match send(
            &mut self.transport,
            &mut self.frame,
            &follow,
            posting.window_id,
        ) {
            Ok(ticket) => posting.ticket = Some(ticket),
            Err(Errno::WouldBlock) => {
                posting.ticket = None;
                posting.held = Some(follow);
            }
            Err(errno) => {
                posting.ticket = None;
                let outcome = read_answer(&mut posting, Err(errno), &mut self.transport);
                self.posted.insert(index, posting);
                return self.answered(index, outcome);
            }
        }
        self.posted.insert(index, posting);
        true
    }

    /// Post the requests that waited for room, in the order they were asked,
    /// stopping at the first the endpoint still has no room for, and answer
    /// whether any went.
    fn post_waiting(&mut self) -> bool {
        let mut posted = false;
        let mut index = 0;
        while index < self.posted.len() {
            let Self {
                transport,
                frame,
                posted: postings,
                ..
            } = self;
            let posting = &mut postings[index];
            if posting.ticket.is_some() {
                index += 1;
                continue;
            }
            let Some(held) = posting.held.as_ref() else {
                // Nothing left to send: what it asked was forgotten.
                postings.remove(index);
                continue;
            };
            match send(transport, frame, held, posting.window_id) {
                Ok(ticket) => {
                    posting.kind = held.kind(posting.kind);
                    posting.ticket = Some(ticket);
                    posting.held = None;
                    posted = true;
                    index += 1;
                }
                Err(Errno::WouldBlock) => break,
                Err(errno) => {
                    posting.held = None;
                    let outcome = read_answer(posting, Err(errno), transport);
                    if self.answered(index, outcome) {
                        index += 1;
                    }
                }
            }
        }
        posted
    }

    /// Send every request waiting for room as a call, oldest first, so one
    /// made after them is served after them: what a call costs a thread that
    /// may wait.
    fn send_waiting_as_calls(&mut self) {
        while let Some(index) = self
            .posted
            .iter()
            .position(|posting| posting.ticket.is_none())
        {
            let Self {
                transport,
                frame,
                reply,
                posted,
                lost,
                ..
            } = self;
            let posting = &mut posted[index];
            let Some(held) = posting.held.take() else {
                posted.remove(index);
                continue;
            };
            posting.kind = held.kind(posting.kind);
            let called = match &held {
                Held::Asked(asked) => asked
                    .request(posting.window_id)
                    .encode(frame)
                    .and_then(|len| transport.call(&frame[..len], reply)),
                Held::Frame(encoded) => transport.call(encoded, reply),
            };
            let answer = match called {
                Ok(len) => reply.get(..len).ok_or(Errno::BufferTooSmall),
                Err(err) => {
                    *lost |= err == Errno::BrokenPipe;
                    Err(err)
                }
            };
            let outcome = read_answer(posting, answer, transport);
            self.answered(index, outcome);
        }
    }

    /// Deliver what every window whose resize waited for room is owed
    /// ([`LastPresent::deferred_event`]), now that room was made, unless a
    /// resize of it is still in flight — whose own answer delivers it.
    fn redeliver_deferred(&mut self) {
        let mut index = 0;
        while let Some(record) = self.presented.get(index).copied() {
            index += 1;
            let in_flight = self.posted.iter().any(|posting| {
                posting.window_id == record.window_id && posting.kind == PostedRequest::Resize
            });
            if !record.deferred || in_flight {
                continue;
            }
            if let Some(stored) = self.record_mut(record.window_id) {
                stored.deferred = false;
            }
            if let Some(event) = record.deferred_event() {
                self.owe(event);
            }
        }
    }

    /// Act on the status `answer` to `posting`.
    fn settle_status(&mut self, posting: &Posting<T::Region>, answer: Result<(), Errno>) {
        match posting.kind {
            // A window, drop or alert the session no longer knows is closed,
            // ended or answered already.
            PostedRequest::LandingEnded(_)
            | PostedRequest::Close
            | PostedRequest::CloseAlert(_) => {}
            PostedRequest::Present => {
                if let Err(errno) = answer {
                    if self.present_refused(posting.window_id, errno) {
                        self.refuse(posting, errno);
                    }
                }
            }
            PostedRequest::Create => {
                if let Err(errno) = answer {
                    self.refuse(posting, errno);
                    // It never existed, so neither did what was asked of it.
                    if !posting.orphaned {
                        self.forget(posting.window_id);
                    }
                }
            }
            PostedRequest::Resize => self.settle_resize(posting, answer),
            _ => {
                if let Err(errno) = answer {
                    self.refuse(posting, errno);
                }
            }
        }
    }

    /// Settle a window's geometry on the `answer` to its resize `posting`.
    fn settle_resize(&mut self, posting: &Posting<T::Region>, answer: Result<(), Errno>) {
        if let Err(errno) = answer {
            self.refuse(posting, errno);
        }
        if posting.orphaned {
            return;
        }
        let Some(record) = self.record_mut(posting.window_id) else {
            return;
        };
        let state = record
            .reported
            .map_or_else(WindowSizeState::default, |(.., state)| state);
        let kept = if answer.is_ok() {
            record.accepted = (record.width_px, record.height_px);
            None
        } else {
            (record.width_px, record.height_px) = record.accepted;
            record.frame_index = None;
            Some(WindowEvent::Resized {
                window_id: posting.window_id,
                width_px: record.accepted.0,
                height_px: record.accepted.1,
                state,
            })
        };
        let owed = if core::mem::take(&mut record.deferred) {
            record.deferred_event()
        } else {
            kept
        };
        if let Some(event) = owed {
            self.owe(event);
        }
    }

    /// Note that a present of window `window_id` was refused: a window whose
    /// frames the session released re-attaches on its next present, and any
    /// other refusal leaves that present declaring the whole window. Answers
    /// whether the application hears of it, which it does not of a release.
    fn present_refused(&mut self, window_id: u64, errno: Errno) -> bool {
        let Some(record) = self.record_mut(window_id) else {
            return true;
        };
        if errno == Errno::NotAttached {
            if record.frames == Frames::Mapped {
                record.frames = Frames::Released;
            }
            return false;
        }
        record.owed_whole = true;
        true
    }

    /// Note that the session released its side of window `window_id`'s
    /// frames because nobody can see it, so the next present re-attaches a
    /// region.
    fn note_released(&mut self, window_id: u64) {
        if let Some(record) = self.record_mut(window_id) {
            record.frames = Frames::Hidden;
        }
    }

    /// `posting`, answered with nothing to go in its place: a window whose
    /// paint waited on its present is owed it now.
    fn retire(&mut self, posting: &Posting<T::Region>) {
        if posting.kind != PostedRequest::Present || posting.orphaned {
            return;
        }
        let window_id = posting.window_id;
        let owed = self
            .record_mut(window_id)
            .is_some_and(|record| core::mem::take(&mut record.frame_owed));
        if owed {
            self.land(Answer::FrameReturned { window_id });
        }
    }

    /// Keep `errno` for the application as the answer to `posting`, unless
    /// its window was closed since.
    fn refuse(&mut self, posting: &Posting<T::Region>, errno: Errno) {
        if posting.orphaned {
            return;
        }
        if self.refused.try_reserve(1).is_err() {
            self.broken = Some(Errno::OutOfMemory);
            return;
        }
        self.refused.push_back(Refused {
            window_id: posting.window_id,
            request: posting.kind,
            errno,
        });
    }

    /// Land `answer` for the application to take.
    fn land(&mut self, answer: Answer) {
        if self.answers.try_reserve(1).is_err() {
            self.broken = Some(Errno::OutOfMemory);
            return;
        }
        self.answers.push_back(answer);
        self.unannounced = true;
    }

    /// Owe the application `event` before its next delivered one.
    fn owe(&mut self, event: WindowEvent) {
        if self.owed.try_reserve(1).is_err() {
            self.broken = Some(Errno::OutOfMemory);
            return;
        }
        self.owed.push_back(event);
    }

    /// Whether a request posted to the session waits for its answer.
    fn awaits_answers(&self) -> bool {
        self.posted.iter().any(|posting| posting.ticket.is_some())
    }

    /// Whether the session's endpoint has gone: once a reap has said so, or
    /// when a reap of the ticket that names no call, which takes nothing,
    /// says so now.
    fn session_lost(&mut self) -> bool {
        self.lost = self.lost
            || self.transport.reap(tairix_abi::CALL_TICKET_NONE, &mut []) == Err(Errno::BrokenPipe);
        self.lost
    }

    /// What the client owes the application before its next delivered
    /// event: a refusal, then geometry to lay out again.
    pub(crate) fn take_owed(&mut self) -> Option<Result<WindowEvent, Refused>> {
        self.reap_posted();
        if let Some(refused) = self.refused.pop_front() {
            return Some(Err(refused));
        }
        self.owed.pop_front().map(Ok)
    }

    /// Encode `request` and post it, answering its ticket.
    fn post(&mut self, request: &WindowRequest) -> Result<u64, Errno> {
        let Self {
            transport, frame, ..
        } = self;
        let len = request.encode(frame)?;
        transport.post(&frame[..len])
    }

    /// The mutable record for `window_id`, if this client owns it.
    fn record_mut(&mut self, window_id: u64) -> Option<&mut LastPresent> {
        self.presented
            .iter_mut()
            .find(|record| record.window_id == window_id)
    }

    /// Issue `request` as a call and decode the shared status reply.
    fn status_call(&mut self, request: &WindowRequest) -> Result<(), Errno> {
        decode_status_reply(self.pull(request)?)
    }

    /// Issue `request` as a call answered into `reply`, returning the reply
    /// length, once whatever waits for room has gone ahead of it.
    ///
    /// The single place a call is encoded, so no call site can send a frame
    /// whose length disagrees with its operation.
    fn call(&mut self, request: &WindowRequest, reply: &mut [u8]) -> Result<usize, Errno> {
        self.send_waiting_as_calls();
        let Self {
            transport, frame, ..
        } = self;
        let len = request.encode(frame)?;
        transport.call(&frame[..len], reply)
    }

    /// [`call`](Self::call) answered into the client's own reply buffer,
    /// answering the reply.
    fn pull(&mut self, request: &WindowRequest) -> Result<&[u8], Errno> {
        let len = self.call_reply(request)?;
        self.reply.get(..len).ok_or(Errno::BufferTooSmall)
    }

    /// [`call`](Self::call) answered into the client's own reply buffer,
    /// answering the reply's length.
    fn call_reply(&mut self, request: &WindowRequest) -> Result<usize, Errno> {
        self.send_waiting_as_calls();
        let Self {
            transport,
            frame,
            reply,
            ..
        } = self;
        let len = request.encode(frame)?;
        transport.call(&frame[..len], reply)
    }
}

/// Post `held` for `window_id`, encoded into `frame` where it is not already.
fn send<T: WindowTransport>(
    transport: &mut T,
    frame: &mut [u8],
    held: &Held,
    window_id: u64,
) -> Result<u64, Errno> {
    match held {
        Held::Asked(asked) => {
            let len = asked.request(window_id).encode(frame)?;
            transport.post(&frame[..len])
        }
        Held::Frame(encoded) => transport.post(encoded),
    }
}

/// `frame`, kept until it can be posted.
fn kept(frame: &[u8]) -> Result<Vec<u8>, Errno> {
    let mut kept = Vec::new();
    kept.try_reserve_exact(frame.len())
        .map_err(|_| Errno::OutOfMemory)?;
    kept.extend_from_slice(frame);
    Ok(kept)
}

/// Damage naming the whole of a `width_px` × `height_px` window.
fn whole(width_px: u32, height_px: u32) -> Result<DamageList, Errno> {
    DamageList::new(&[DamageRect {
        x: 0,
        y: 0,
        width_px,
        height_px,
    }])
}

/// The present `newer` held behind the present of frame `earlier`, held
/// before it, in a window of `extent`: the same frame drawn again declares
/// what both changed, and another frame the whole window, since what it lacks
/// of the first is unknown.
fn merged_present(
    (frame, damage): (u32, DamageList),
    newer: Asked,
    extent: Option<(u32, u32)>,
) -> Asked {
    let Asked::Present(newer_frame, newer_damage) = newer else {
        return newer;
    };
    let both = if newer_frame == frame {
        DamageList::fitted(damage.rects().iter().chain(newer_damage.rects()).copied())
    } else {
        extent.and_then(|(width_px, height_px)| whole(width_px, height_px).ok())
    };
    Asked::Present(newer_frame, both.unwrap_or(newer_damage))
}

/// `text`, owned.
fn owned(text: &str) -> Result<String, Errno> {
    let mut owned = String::new();
    owned
        .try_reserve_exact(text.len())
        .map_err(|_| Errno::OutOfMemory)?;
    owned.push_str(text);
    Ok(owned)
}

/// UTF-8 `bytes`, owned.
fn owned_text(bytes: &[u8]) -> Result<String, Errno> {
    owned(core::str::from_utf8(bytes).map_err(|_| Errno::OutOfRange)?)
}

/// The name `bytes` spell, owned, or none where it cannot be kept: a name
/// travels with a delegation, which is never let go over its name.
fn name_or_empty(bytes: &[u8]) -> String {
    owned_text(bytes).unwrap_or_default()
}

/// What the `answer` to `posting` came to, as its kind reads one. A paste
/// whose payload outgrew the region offered is asked again through a region
/// `transport` grants of the length the session stated.
fn read_answer<T: WindowTransport>(
    posting: &mut Posting<T::Region>,
    answer: Result<&[u8], Errno>,
    transport: &mut T,
) -> Outcome {
    let window_id = posting.window_id;
    match posting.kind {
        PostedRequest::OfferedFile(serial) => Outcome::landed(Answer::OfferedFile(OfferedMedia {
            window_id,
            serial,
            media: answer.and_then(decode_offered_file_reply).and_then(owned),
        })),
        PostedRequest::HandOver(token) => Outcome::landed(Answer::HandOver {
            token,
            outcome: answer.and_then(decode_hand_over_reply),
        }),
        PostedRequest::RevealDocument(token) => Outcome::landed(Answer::Revealed {
            token,
            shown: answer.and_then(decode_status_reply),
        }),
        PostedRequest::OpenTarget => read_open_target(answer),
        PostedRequest::MenuText(open_id) => Outcome::landed(Answer::MenuText {
            window_id,
            open_id,
            text: answer
                .and_then(decode_menu_text_reply)
                .and_then(|text| text.map(owned).transpose()),
        }),
        PostedRequest::AppBarText(item, revision) => Outcome::landed(Answer::AppBarText {
            item,
            revision,
            text: answer
                .and_then(decode_menu_text_reply)
                .and_then(|text| text.map(owned).transpose()),
        }),
        PostedRequest::DragSpot(serial) => Outcome::landed(Answer::DragSpot {
            window_id,
            serial,
            folder: answer.and_then(decode_drag_spot_reply).and_then(owned),
        }),
        PostedRequest::DropTarget => Outcome::landed(Answer::DropTarget {
            window_id,
            target: answer
                .and_then(decode_drop_target_reply)
                .and_then(|target| {
                    Ok(Dropped {
                        run_path: owned(target.run_path.as_str())?,
                        writes_documents: target.writes_documents,
                    })
                }),
        }),
        PostedRequest::PickedName => Outcome::landed(Answer::PickedName {
            window_id,
            name: answer
                .and_then(decode_picked_name_reply)
                .and_then(|name| owned(name.as_str())),
        }),
        PostedRequest::PickedFile => {
            let file = answer
                .and_then(decode_picked_file_reply)
                .map(|file| Picked {
                    handle: file.handle,
                    name: name_or_empty(file.name.as_str().as_bytes()),
                });
            let read_on = file.is_ok().then_some(ReadOn::PickedFile);
            Outcome::Read(Some(Answer::PickedFile { window_id, file }), read_on)
        }
        PostedRequest::OfferedItems(drag) => read_items(posting, drag, answer),
        PostedRequest::Clipboard => read_clipboard(posting, answer, transport),
        PostedRequest::Terrain => Outcome::landed(Answer::Terrain {
            window_id,
            plates: answer.and_then(read_terrain),
        }),
        _ => Outcome::Status(answer.and_then(decode_status_reply)),
    }
}

/// The open target `answer` brings, and the queue read on after it: a
/// target the client could not read is landed as the error and the queue
/// still read on, since the session has given it up.
fn read_open_target(answer: Result<&[u8], Errno>) -> Outcome {
    match answer.and_then(decode_open_target_reply) {
        Ok(Some(target)) => Outcome::Read(
            Some(Answer::OpenTarget(Target::of(target).map(Some))),
            Some(ReadOn::OpenTarget),
        ),
        drained => Outcome::landed(Answer::OpenTarget(drained.map(|_| None))),
    }
}

/// A page of a drag's items `answer` brings, gathered into `posting`'s, and
/// the page read on after it; the whole once the last page is in.
fn read_items<R>(posting: &mut Posting<R>, drag: u32, answer: Result<&[u8], Errno>) -> Outcome {
    let window_id = posting.window_id;
    let landed = |items| {
        Outcome::landed(Answer::OfferedItems {
            window_id,
            drag,
            items,
        })
    };
    let Carried::Items(items) = &mut posting.carried else {
        return landed(Err(Errno::OutOfRange));
    };
    let page = match answer.and_then(decode_offered_items_reply) {
        Ok(page) => page,
        Err(errno) => return landed(Err(errno)),
    };
    if items.try_reserve(usize::from(page.len())).is_err() {
        return landed(Err(Errno::OutOfMemory));
    }
    for item in page.iter() {
        match owned(item.path) {
            Ok(path) => items.push(OfferedPath { path, id: item.id }),
            Err(errno) => return landed(Err(errno)),
        }
    }
    let Ok(from) = u32::try_from(items.len()) else {
        return landed(Err(Errno::OutOfRange));
    };
    if from >= page.total() {
        return landed(Ok(core::mem::take(items)));
    }
    // A page that brings nothing it owes would read on for ever.
    if page.is_empty() {
        return landed(Err(Errno::OutOfRange));
    }
    Outcome::Read(None, Some(ReadOn::OfferedItems(drag, from)))
}

/// What a paste's `answer` brings out of `posting`'s region, or the paste
/// asked again through a region `transport` grants of the length stated.
fn read_clipboard<T: WindowTransport>(
    posting: &mut Posting<T::Region>,
    answer: Result<&[u8], Errno>,
    transport: &mut T,
) -> Outcome {
    let window_id = posting.window_id;
    let landed = |contents| {
        Outcome::landed(Answer::Clipboard {
            window_id,
            contents,
        })
    };
    let Carried::Region(region, second) = &mut posting.carried else {
        return landed(Err(Errno::OutOfRange));
    };
    let held = match answer.and_then(decode_clipboard_reply) {
        Ok(held) => held,
        Err(errno) => return landed(Err(errno)),
    };
    match clipboard::landed(held, region.bytes_mut(), *second) {
        Ok(Landing::Taken(contents)) => landed(Ok(contents)),
        Ok(Landing::Again(len)) => match transport.grant_region(len) {
            Ok((fresh, shm_handle)) => {
                posting.carried = Carried::Region(fresh, true);
                Outcome::Read(None, Some(ReadOn::Clipboard(shm_handle)))
            }
            Err(errno) => landed(Err(errno)),
        },
        Err(errno) => landed(Err(errno)),
    }
}

/// The plates a terrain reply carries, owned.
fn read_terrain(bytes: &[u8]) -> Result<Vec<TerrainPlate>, Errno> {
    let none = TerrainPlate {
        x: 0,
        y: 0,
        width_px: 0,
        height_px: 0,
    };
    let mut decoded = [none; DESKTOP_LAYER_MAX_PLATES as usize];
    let plates = decode_terrain_reply(bytes, &mut decoded)?;
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(plates.len())
        .map_err(|_| Errno::OutOfMemory)?;
    owned.extend_from_slice(plates);
    Ok(owned)
}

impl Target {
    /// `target`, owned.
    fn of(target: OpenTarget<'_>) -> Result<Self, Errno> {
        Ok(match target {
            OpenTarget::Path(path) => Self::Path(owned_text(path)?),
            OpenTarget::Image(path) => Self::Image(owned_text(path)?),
            OpenTarget::Document {
                name,
                grant,
                writable,
            } => Self::Document {
                name: name_or_empty(name),
                grant,
                writable,
            },
            OpenTarget::Pane(pane) => Self::Pane(owned_text(pane)?),
            OpenTarget::Placed {
                window_id,
                x,
                y,
                name,
                grant,
            } => Self::Placed {
                window_id,
                at: (x, y),
                name: name_or_empty(name),
                grant,
            },
        })
    }
}

impl Answer {
    /// The window the answer is about, if it is about one.
    const fn window_id(&self) -> Option<u64> {
        match self {
            Self::OfferedFile(media) => Some(media.window_id),
            Self::MenuText { window_id, .. }
            | Self::DragSpot { window_id, .. }
            | Self::DropTarget { window_id, .. }
            | Self::PickedName { window_id, .. }
            | Self::PickedFile { window_id, .. }
            | Self::OfferedItems { window_id, .. }
            | Self::Clipboard { window_id, .. }
            | Self::Terrain { window_id, .. }
            | Self::FrameReturned { window_id } => Some(*window_id),
            Self::OpenTarget(_)
            | Self::AppBarText { .. }
            | Self::HandOver { .. }
            | Self::Revealed { .. } => None,
        }
    }
}

/// The window an authorisation names, `None` asking for the icon-bar slot.
///
/// # Errors
///
/// [`Errno::OutOfRange`] for window `0`: the wire spells the slot so, and a
/// window id left unset must not ask for it.
fn asked_window(window_id: Option<u64>) -> Result<Option<core::num::NonZeroU64>, Errno> {
    window_id
        .map(|id| core::num::NonZeroU64::new(id).ok_or(Errno::OutOfRange))
        .transpose()
}

/// The drain half of the event-arrival seam: read a queued event frame,
/// never wait for one.
///
/// Stated apart from [`EventSource`] because parking is not every loop's to
/// do. An app whose wait-set carries several wake sources and a frame
/// deadline dispatches them itself, so it owns the park and wants only the
/// drain — and [`WindowEvents`] is bounded on *this* trait, so such a loop
/// still reads through the one shared stream discipline instead of spelling
/// a raw mailbox drain of its own.
pub trait EventDrain {
    /// Fill `event` with a queued event frame if one is waiting, answering
    /// `Ok(false)` immediately when the mailbox is empty.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the drain surfaces (the endpoint torn down, the session
    /// gone); the app treats it as the channel ending.
    fn try_next(&mut self, event: &mut [u8; WindowEvent::WIRE_LEN]) -> Result<bool, Errno>;
}

/// The event-arrival seam: the app's own event mailbox, split into the drain
/// that never waits and the park that only waits.
///
/// A loop that has nothing else to do calls [`WindowEvents::wait`], which
/// alternates the two. A loop that interleaves input with work of its own — a
/// thumbnail to render, an icon to decode — drains with
/// [`WindowEvents::try_wait`] so queued input is served before the next unit
/// of that work, and parks only once both are exhausted. Both halves are the
/// mailbox's, so the polled path and the parked one cannot drift apart.
pub trait EventSource: EventDrain {
    /// Park the task until something the app waits on is ready — a delivered
    /// event, an answer to a posted request, or whatever else the
    /// implementation's wait-set carries.
    ///
    /// A wake is not a promise of an event, so the answer says whether the
    /// caller must regain control ([`Parked::Interrupted`]) or the source
    /// handled it and the wait should go round again ([`Parked::Served`]).
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the wait surfaces; the app treats it as the channel
    /// ending.
    fn park(&mut self) -> Result<Parked, Errno>;
}

/// Why reading the next event failed.
///
/// The two are answered apart because the caller's response to them is
/// opposite and an [`Errno`] alone cannot tell them apart: the drain surfaces
/// the kernel's codes, one of which ([`Errno::LengthOutOfRange`]) a decode
/// refusal also uses. Reading on past a failed *drain* meets the same failure
/// at once and spins.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EventError {
    /// The session delivered a frame this build will not decode. It is
    /// already consumed and nothing was guessed at, so the next read moves
    /// past it.
    Undecodable(Errno),
    /// The mailbox itself failed — the endpoint torn down, the session gone.
    /// It will fail the same way again; the app treats the channel as ended.
    Mailbox(Errno),
    /// The session refused a request the client posted. The application
    /// reports it as it would a refused call, and the stream goes on; a
    /// refused resize is followed by the size the session kept.
    Refused(Refused),
}

/// What a [`park`](EventSource::park) woke for.
///
/// A loop with a worker parks on that worker's wake alongside its event
/// mailbox, and the answer is the loop's to adopt — it must therefore be able
/// to leave the wait without an event. Reporting every wake as "no event yet"
/// instead would park again on a wake source that is *still* ready, which is
/// a spin, not a wait.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Parked {
    /// The source dealt with the wake itself — a memory-pressure trim, a
    /// reaped child — or it is the event stream's to take: a delivered event,
    /// or an answer to a posted request, which the wait takes in before it
    /// parks again. Nothing is owed to the loop.
    Served,
    /// Something the loop owns woke it, and the source has consumed the
    /// readiness that reported it. The wait ends with no event so the loop can
    /// act on whatever landed.
    Interrupted,
}

/// The app's typed event stream over an [`EventDrain`].
///
/// Every app reads here rather than from its mailbox, so the one folding rule
/// below applies to all of them. [`wait`](Self::wait) is additionally offered
/// where the source can park ([`EventSource`]); a loop that parks on its own
/// wait-set uses [`try_wait`](Self::try_wait) alone.
pub struct WindowEvents<S: EventDrain> {
    source: S,
    /// The one frame read ahead while folding a run of resizes and found
    /// not to belong to it. Returned before the source is drained again, so
    /// folding never reorders or drops an event.
    read_ahead: Option<[u8; WindowEvent::WIRE_LEN]>,
    /// The next drain follows a wake ([`woke`](Self::woke)).
    woken: bool,
}

impl<S: EventDrain> WindowEvents<S> {
    /// A typed stream over `source`.
    pub const fn new(source: S) -> Self {
        Self {
            source,
            read_ahead: None,
            woken: false,
        }
    }

    /// Note that the application's own wait returned. The drain after it that
    /// finds nothing, with no answer owed, asks whether the session is still
    /// there: the window channel's wait stays ready once it has gone, so the
    /// wait would otherwise return at once for ever. [`wait`](Self::wait)
    /// notes its own wakes.
    pub fn woke(&mut self) {
        self.woken = true;
    }

    /// The queued event if one is waiting, and `None` at once when the mailbox
    /// is empty — decoded and redraw-answered exactly as [`wait`](Self::wait)
    /// does.
    ///
    /// What a loop with work of its own drains with: input is served before
    /// the next unit of that work, and the loop parks on [`wait`](Self::wait)
    /// only once both are exhausted.
    ///
    /// # Errors
    ///
    /// [`EventError::Mailbox`] for a drain failure, or with
    /// [`Errno::BrokenPipe`] once the session is found gone, or
    /// [`EventError::Undecodable`] for a malformed frame — refused, never
    /// guessed at, exactly as in [`wait`](Self::wait).
    pub fn try_wait<T: WindowTransport>(
        &mut self,
        client: &mut WindowClient<T>,
    ) -> Result<Option<WindowEvent>, EventError> {
        if !core::mem::take(&mut self.woken) {
            return self.drain(client);
        }
        let owed = client.awaits_answers();
        let event = self.drain(client)?;
        if event.is_none() && !owed && client.session_lost() {
            return Err(EventError::Mailbox(Errno::BrokenPipe));
        }
        Ok(event)
    }

    /// The next event: an owed answer or refusal first, then the mailbox.
    fn drain<T: WindowTransport>(
        &mut self,
        client: &mut WindowClient<T>,
    ) -> Result<Option<WindowEvent>, EventError> {
        if let Some(errno) = client.broken {
            return Err(EventError::Mailbox(errno));
        }
        match client.take_owed() {
            Some(Ok(event)) => return Ok(Some(event)),
            Some(Err(refused)) => return Err(EventError::Refused(refused)),
            None => {}
        }
        let mut frame = [0u8; WindowEvent::WIRE_LEN];
        if let Some(held) = self.read_ahead.take() {
            frame = held;
        } else if !self
            .source
            .try_next(&mut frame)
            .map_err(EventError::Mailbox)?
        {
            if client.lost {
                return Err(EventError::Mailbox(Errno::BrokenPipe));
            }
            return Ok(None);
        }
        self.fold_resizes(&mut frame)?;
        decode_delivered(client, &frame).map(Some)
    }

    /// Advance `frame` past an unbroken run of [`WindowEvent::Resized`] for
    /// the same window, leaving the newest one in it.
    ///
    /// A client extent is a value the window converges on, not an occurrence
    /// it must witness: an interactive resize-grab reports one per pointer
    /// sample, and an app that re-laid-out and re-mapped for each of them
    /// would do that work once per queued sample to arrive at the size the
    /// last one already named. Only a *consecutive* run folds — the frame
    /// that ends it is put back untouched — so nothing is reordered and no
    /// other event is lost. The reads are non-blocking, so a fold never
    /// waits for an event that may not come.
    ///
    /// A frame that will not decode ends the run and is put back, so its
    /// typed refusal reaches the caller on the next drain rather than being
    /// swallowed here.
    fn fold_resizes(&mut self, frame: &mut [u8; WindowEvent::WIRE_LEN]) -> Result<(), EventError> {
        let Some(window) = resized_window(frame) else {
            return Ok(());
        };
        let mut ahead = [0u8; WindowEvent::WIRE_LEN];
        while self
            .source
            .try_next(&mut ahead)
            .map_err(EventError::Mailbox)?
        {
            if resized_window(&ahead) != Some(window) {
                self.read_ahead = Some(ahead);
                return Ok(());
            }
            *frame = ahead;
        }
        Ok(())
    }
}

impl<S: EventSource> WindowEvents<S> {
    /// Park until the next event arrives, decode it, and answer a
    /// redraw request on the app's behalf before returning it.
    ///
    /// The session releases a window's retained pixels under memory
    /// pressure and asks for them again with
    /// [`WindowEvent::RedrawRequested`].
    /// Answering it is mechanical — re-present the last frame with
    /// full-window damage — so the library does it through `client`
    /// ([`WindowClient::answer_redraw`]) and no app has to. A failed
    /// re-present is not fatal: the window stays blank until the app
    /// presents for a reason of its own, which is exactly the outcome
    /// the event already documents.
    ///
    /// The event is still returned, so an app that would rather re-render
    /// its content genuinely (a live view whose last frame is already
    /// out of date) can act on it.
    ///
    /// Every park is preceded by a [`try_wait`](Self::try_wait), so an answer
    /// to a posted request is taken in before the wait parks again: the
    /// wait-set's reply member stays ready until it is, and a park reporting
    /// it again would return at once, for ever.
    ///
    /// It answers `None` with no event when the park was interrupted, the
    /// loop owning what woke it, or once an answer has landed for the
    /// application to take ([`WindowClient::take_answer`]): once for each
    /// landing, so a loop that leaves an answer untaken parks again rather
    /// than spinning on it.
    ///
    /// # Errors
    ///
    /// [`EventError::Mailbox`] for a source failure or a session that has
    /// gone ([`woke`](Self::woke)),
    /// [`EventError::Undecodable`] for a malformed frame — a corrupt event is
    /// refused, never guessed at — or [`EventError::Refused`] for a posted
    /// request the session refused. Only the first ends the channel; a frame
    /// is already consumed either way.
    pub fn wait<T: WindowTransport>(
        &mut self,
        client: &mut WindowClient<T>,
    ) -> Result<Option<WindowEvent>, EventError> {
        if let Some(event) = self.try_wait(client)? {
            return Ok(Some(event));
        }
        loop {
            if core::mem::take(&mut client.unannounced) {
                return Ok(None);
            }
            match self.source.park().map_err(EventError::Mailbox)? {
                Parked::Served => {}
                // The loop owns what woke it and has no event to handle.
                Parked::Interrupted => return Ok(None),
            }
            self.woken = true;
            if let Some(event) = self.try_wait(client)? {
                return Ok(Some(event));
            }
        }
    }
}

/// The window a delivered frame reports a new client extent for, or `None`
/// when it is any other event (or will not decode).
fn resized_window(frame: &[u8; WindowEvent::WIRE_LEN]) -> Option<u64> {
    match WindowEvent::from_bytes(frame) {
        Ok(WindowEvent::Resized { window_id, .. }) => Some(window_id),
        _ => None,
    }
}

/// Decode one delivered frame and answer a redraw request on the app's behalf.
/// The one definition both the parked and the drained path use, so neither can
/// answer a redraw the other would not.
fn decode_delivered<T: WindowTransport>(
    client: &mut WindowClient<T>,
    frame: &[u8; WindowEvent::WIRE_LEN],
) -> Result<WindowEvent, EventError> {
    let event = WindowEvent::from_bytes(frame).map_err(EventError::Undecodable)?;
    match event {
        WindowEvent::RedrawRequested { window_id } => {
            if let Some(record) = client
                .record_mut(window_id)
                .filter(|record| record.frames == Frames::Hidden)
            {
                record.frames = Frames::Released;
            }
            let _ = client.answer_redraw(window_id);
        }
        WindowEvent::ContentReleased { window_id } => client.note_released(window_id),
        _ => {}
    }
    client.note_reported(&event);
    Ok(event)
}

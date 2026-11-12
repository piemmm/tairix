//! The server half: the engine the desktop session composes.
//!
//! [`WindowServer::serve`] handles exactly one received request:
//! decode → caller attestation → owner/bounds validation → action →
//! encoded reply. The hosting session owns the transport (bind the
//! reserved `WINDOW_ENDPOINT`, park on its wait-set, `call_recv` /
//! `call_reply`); the engine owns every decision in between, so the
//! semantics are host-testable against mock seams and identical under
//! any host.
//!
//! # Security shape
//!
//! * **Identity is kernel-attested.** The engine never reads a claimed
//!   owner: [`CallerIdentity::caller`] asks the kernel who the
//!   *in-flight caller of this endpoint* is (`call_peer_origin`'s
//!   unforgeable `ProcId`), and every request is attributed to it
//!   before any state is read or mutated. A kernel-domain caller is
//!   refused outright — a window belongs to a user process instance.
//! * **Windows are keyed to their creator.** A `Present` or `Close`
//!   naming a window the caller does not own answers `NotFound`,
//!   indistinguishable from a window that never existed, so the id
//!   space leaks nothing about other apps' windows.
//! * **Every bound is checked before any pixel access.** The mapped
//!   region must hold every frame at create time, the frame index must
//!   name a created frame, and the damage rectangle must lie inside the
//!   window's surface; each failure is a typed refusal, never a clamp.
//! * **A hostile client is bounded.** At most
//!   [`client_frame_budget_bytes`] of mapped window frame per attested
//!   client — bytes, not windows, because that is the resource a window
//!   actually spends here — so one app cannot pin unbounded shared memory
//!   or flood the taskbar, however large or numerous its windows.
//! * **Teardown fails closed.** [`WindowServer::client_exited`] drops a
//!   dead client's windows (and their region mappings) and tells the
//!   host, so an exited app never leaks a mapped grant or a ghost
//!   taskbar entry.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::desktop::DesktopInfo;
use tairix_abi::driver::display::{
    DamageList, DamageRect, DisplayFormat, DisplayMode, MAX_DAMAGE_RECTS,
};
use tairix_abi::origin::{AppIdentity, ProcId};
use tairix_abi::reply::{encode_status_reply, STATUS_REPLY_LEN};
use tairix_abi::window_ipc::{
    encode_authorised_output_reply, encode_screensaver_reply, AppMenuItemId, AuthorisedOutcome,
    SystemTask, APP_MENU_MAX_TOTAL_ROWS, WINDOW_AUTHORISED_OUTPUT_REPLY_MAX,
    WINDOW_SCREENSAVER_REPLY_MAX,
};
use tairix_abi::window_ipc::{
    encode_clipboard_reply, encode_connect_reply, encode_cursor_sets_reply, encode_drag_spot_reply,
    encode_drop_target_reply, encode_hand_over_reply, encode_handle_reply, encode_menu_text_reply,
    encode_notify_sources_reply, encode_offered_file_reply, encode_offered_items_reply,
    encode_open_target_reply, encode_picked_file_reply, encode_picked_name_reply,
    encode_terrain_reply, encode_wallpapers_reply, AlertOutcome, AlertSpec, AppBar, AppMenu,
    ClipboardHeld, ClipboardKind, CursorShape, DialogModality, DocumentName, DragAt, DragItems,
    DropOperation, DropSite, DropTarget, HandOverOutcome, HandOverTarget, LayerDepth, ObjectIds,
    OfferDrop, OfferedItem, OpenTarget, PickPurpose, PreviewSubject, TerrainPlate, WallpaperEntry,
    WindowEvent, WindowRegion, WindowRequest, WindowTitle, ALERT_MAX_OPEN, APP_MENU_ENTRY_MAX,
    DESKTOP_LAYER_MAX_PER_CLIENT, DESKTOP_LAYER_MAX_PER_SEAT, DESKTOP_LAYER_MAX_PLATES,
    OBJECT_RANGES_MAX, WINDOW_CLIPBOARD_REPLY_LEN, WINDOW_CONNECT_REPLY_LEN,
    WINDOW_CURSOR_SETS_REPLY_MAX, WINDOW_DRAG_SPOT_REPLY_MAX, WINDOW_DROP_TARGET_REPLY_MAX,
    WINDOW_FOLDER_PICK_MAX, WINDOW_HANDLE_REPLY_LEN, WINDOW_HAND_OVER_REPLY_LEN,
    WINDOW_MAX_OPEN_TARGETS, WINDOW_MENU_TEXT_REPLY_MAX, WINDOW_NOTIFY_SOURCES_REPLY_MAX,
    WINDOW_OFFERED_FILE_REPLY_MAX, WINDOW_OFFERED_ITEMS_REPLY_MAX, WINDOW_OPEN_TARGET_REPLY_MAX,
    WINDOW_PICKED_FILE_REPLY_MAX, WINDOW_PICKED_NAME_REPLY_MAX, WINDOW_TERRAIN_REPLY_MAX,
    WINDOW_WALLPAPERS_REPLY_MAX,
};
use tairix_abi::window_ipc::{KeyboardMode, LandingStep, DROP_LANDINGS_MAX};
pub use tairix_abi::window_ipc::{WindowSizeState, WindowSizing};
use tairix_abi::{BundleId, CapabilityId, Errno};
use tairix_display::{add_damage, damage_list, FrameRegion, ShmMapper};
use tairix_geometry::{Rect, Region};

/// Upper bound, in bytes, of any reply [`WindowServer::serve`] writes,
/// so one fixed buffer holds every outcome: the open-target frame (the
/// widest, since a path is), the create frame, the desktop frame, the
/// hand-over frame, or the status frame that fits inside any of them.
///
/// Derived from every reply the channel has rather than from the ones that
/// happen to be widest today, so an operation whose reply outgrew the buffer
/// could not slip past.
///
/// A caller holds this buffer **once** for the life of its serve loop rather
/// than taking one per request: it is sized to the widest reply the channel
/// has, and a per-request array would cost a present — the hottest operation
/// and one of the shortest — the whole of the widest one's clearing.
pub const WINDOW_REPLY_MAX: usize = {
    const fn wider(a: usize, b: usize) -> usize {
        if a > b {
            a
        } else {
            b
        }
    }
    wider(
        wider(STATUS_REPLY_LEN, WINDOW_CONNECT_REPLY_LEN),
        wider(
            wider(WINDOW_OPEN_TARGET_REPLY_MAX, WINDOW_HAND_OVER_REPLY_LEN),
            wider(
                wider(WINDOW_HANDLE_REPLY_LEN, WINDOW_MENU_TEXT_REPLY_MAX),
                wider(
                    WINDOW_TERRAIN_REPLY_MAX,
                    wider(
                        wider(WINDOW_WALLPAPERS_REPLY_MAX, WINDOW_SCREENSAVER_REPLY_MAX),
                        wider(
                            WINDOW_CURSOR_SETS_REPLY_MAX,
                            wider(
                                WINDOW_NOTIFY_SOURCES_REPLY_MAX,
                                wider(
                                    wider(
                                        WINDOW_PICKED_NAME_REPLY_MAX,
                                        WINDOW_PICKED_FILE_REPLY_MAX,
                                    ),
                                    wider(
                                        wider(
                                            WINDOW_DROP_TARGET_REPLY_MAX,
                                            wider(
                                                WINDOW_DRAG_SPOT_REPLY_MAX,
                                                wider(
                                                    WINDOW_OFFERED_ITEMS_REPLY_MAX,
                                                    WINDOW_OFFERED_FILE_REPLY_MAX,
                                                ),
                                            ),
                                        ),
                                        WINDOW_AUTHORISED_OUTPUT_REPLY_MAX,
                                    ),
                                ),
                            ),
                        ),
                    ),
                ),
            ),
        ),
    )
};

/// What [`WindowServer::serve`] made of one request.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Served {
    /// Answer the call now with this many bytes of the reply buffer.
    Reply(usize),
    /// The answer is withheld: a present whose window holds a frame the
    /// desktop has yet to show, answered through
    /// [`WindowServer::send_answers`] once that frame is shown or refused, or
    /// something supersedes it.
    Withheld,
}

/// Where a carried drag is, as the host reports it ([`WindowServer::report_drag`]).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DragReport<'a> {
    /// Over one of the dragging application's own windows, at a point in it.
    Window {
        /// The window under the pointer.
        window_id: u64,
        /// Window-local x.
        x: u32,
        /// Window-local y.
        y: u32,
    },
    /// Over the desktop, on the folder at the absolute path `folder`.
    Desktop {
        /// The folder a drop there would land in.
        folder: &'a str,
    },
    /// Over nothing the application can drop on.
    Nowhere,
}

/// Where a carried drag ended, as the host concludes it
/// ([`WindowServer::conclude_drag`]).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DragConclusion {
    /// On nothing that takes it.
    Nothing,
    /// On an application that opens the one file dragged. Boxed: a run path
    /// is far wider than every other conclusion.
    Application(Box<DropTarget>),
    /// On one of the dragging application's own windows.
    Window {
        /// The window dropped on.
        window_id: u64,
        /// Window-local x.
        x: u32,
        /// Window-local y.
        y: u32,
        /// What the drop does: the verdict the pointer showed.
        operation: DropOperation,
    },
    /// On the desktop folder the report numbered `serial` named.
    Desktop {
        /// The report that named the folder.
        serial: u32,
        /// What the drop does: the verdict the pointer showed.
        operation: DropOperation,
    },
    /// On another application's window that answered it places the one file
    /// dragged there.
    Placed(Placement),
}

/// Where a file a drag placed goes: another application's window and the
/// window-local point it was dropped at.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Placement {
    /// The window dropped on.
    pub window_id: u64,
    /// Window-local x.
    pub x: u32,
    /// Window-local y.
    pub y: u32,
}

/// The file a [`WindowRequest::PlaceDropped`] hands over: its name for
/// display, and the delegation its dragging application minted to the
/// session.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PlacedFile<'a> {
    /// Its own file name, for display.
    pub name: &'a DocumentName,
    /// The `fd_grant` handle minted to the session.
    pub grant: core::num::NonZeroU64,
}

/// What the last drag's drop leaves its owner to take.
#[derive(Clone, Debug, Eq, PartialEq)]
enum DropHeld {
    /// The application it opens in ([`WindowRequest::TakeDropTarget`]),
    /// boxed as the conclusion carries it: a run path is far wider than a
    /// placement, and every window holds one of these.
    Target(Box<DropTarget>),
    /// The window it is placed in ([`WindowRequest::PlaceDropped`]).
    Placed(Placement),
}

const _: () = assert!(WINDOW_CLIPBOARD_REPLY_LEN <= WINDOW_REPLY_MAX);
const _: () = assert!(WINDOW_AUTHORISED_OUTPUT_REPLY_MAX <= WINDOW_REPLY_MAX);

/// The share of the machine's RAM one attested client may hold mapped in the
/// session as window frames: half of it.
///
/// The *physical* memory behind those frames is the client's own allocation,
/// already bounded by its address-space limit and by the machine; what this
/// bound protects is the session, which must not let one client's windows
/// crowd out every other client's ability to map and draw. Each window holds
/// two frames, so half is what fits fifty terminal windows on a 256 MiB board
/// and thirty full-screen ones on a 4 GiB machine driving a 4K display. A
/// share admitting fewer, as an eighth of single frames did when it refused
/// the twenty-seventh terminal window on that board, is a bound tighter than
/// the machine rather than a defence.
const CLIENT_FRAME_RAM_DIVISOR: u64 = 2;

/// Frames of the session's own output one client may hold when the machine's
/// RAM total is unknown.
///
/// A window is clamped to the screen, so what a client can *show* is one
/// frame; the rest is stacked-under, minimised, and double-buffered windows.
/// Enough of those that no ordinary desktop meets the bound, and still a bound.
const CLIENT_FRAME_SCREENFULS: u64 = 32;

/// Why a pick was refused past one of the bounds on the picks open: the
/// window's, the application's, and its folder pick's.
const PICK_ON_WINDOW: &str = "the window has a pick open";
const PICKS_PER_APPLICATION: &str = "the application has as many picks open as it may";
const FOLDER_PICK_OPEN: &str = "the application has a folder pick open";

/// The bytes of window frame one attested client may hold mapped in the
/// session at once.
///
/// A **validation bound**, not a capacity, and deliberately measured in bytes
/// rather than windows: each window pins its shared frame region in the
/// session's address space, and a count says nothing about how much that is.
/// Thirty-two windows of a 4K frame is a gigabyte, while a hundred terminal
/// windows are a few tens of megabytes — a count generous enough for the
/// second is catastrophic for the first, and one tight enough for the first
/// refuses the second for no reason. Bounding the bytes bounds the resource
/// actually at stake, and the number of windows follows from how big the ones
/// a client opens really are. It also covers a resize, which a count cannot
/// see at all.
///
/// Both inputs are discovered, never hand-picked: `total_ram_bytes` is the
/// machine's usable physical RAM (the System Information API's total) and
/// `output_frame_bytes` is one frame of the session's own display mode. RAM is
/// the resource at stake so it decides wherever it is known; a total of zero
/// means the query went unanswered, and falling back to the display keeps the
/// desktop working under a bound instead of refusing every window.
#[must_use]
pub fn client_frame_budget_bytes(total_ram_bytes: u64, output_frame_bytes: usize) -> u64 {
    let from_ram = total_ram_bytes / CLIENT_FRAME_RAM_DIVISOR;
    if from_ram > 0 {
        return from_ram;
    }
    (output_frame_bytes as u64).saturating_mul(CLIENT_FRAME_SCREENFULS)
}

/// The caller-attestation seam — the kernel's `call_peer_origin` behind
/// a trait, so the engine is host-testable and never trusts a claimed
/// identity.
pub trait CallerIdentity {
    /// The kernel-attested [`ProcId`] of the in-flight caller identified
    /// by `ticket`.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the attestation surfaces (a vanished caller, a
    /// transport fault); the engine refuses the request in that case.
    fn caller(&mut self, ticket: u64) -> Result<ProcId, Errno>;

    /// The event mailbox the in-flight caller behind `ticket` binds for its
    /// window events — [`event_endpoint_for`](tairix_abi::window_ipc::event_endpoint_for)
    /// its attested pid — and so the only one its requests may name: one
    /// application's events are never aimed at another's mailbox.
    ///
    /// # Errors
    ///
    /// As [`caller`](Self::caller).
    fn caller_mailbox(&mut self, ticket: u64) -> Result<u64, Errno>;

    /// Whether the in-flight caller behind `ticket` holds `cap`, from the
    /// **kernel's** attestation of that caller rather than anything the
    /// caller said.
    ///
    /// The channel is bound unrestricted-sender, because a session is an
    /// ordinary user process and the kernel reserves a restricted-sender
    /// bind for holders of `CAP_IPC_BIND_PRIVILEGED` — a seat lease
    /// substitutes only for the reserved-id half of that gate. So the one
    /// gated operation on this channel is checked here instead, against the
    /// same unforgeable fact the kernel would have used.
    ///
    /// The default answers `false`: an identity source that cannot attest
    /// capabilities must not be taken to grant them, so a host without one
    /// refuses the privileged operations rather than opening them.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the attestation surfaces; the engine refuses the
    /// request in that case.
    fn caller_holds(&mut self, ticket: u64, cap: CapabilityId) -> Result<bool, Errno> {
        let _ = (ticket, cap);
        Ok(false)
    }

    /// The application the kernel attests the in-flight caller behind
    /// `ticket` is running, or `None` when it runs no verified bundle.
    ///
    /// The default answers `None`: an identity source that cannot attest an
    /// application must not be taken to name one, so a request reserved for
    /// one application is refused rather than opened.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the attestation surfaces; the engine refuses the
    /// request in that case.
    fn caller_app(&mut self, ticket: u64) -> Result<Option<AppIdentity>, Errno> {
        let _ = ticket;
        Ok(None)
    }
}

/// The session's compositor bridge: what the engine tells the desktop
/// about window lifecycle and presented pixels.
///
/// The engine has already validated everything it hands over — the
/// geometry, the ownership, the frame slice length
/// (`surface.stride_bytes * surface.height_px`), and the damage
/// rectangle's bounds — so an implementation only composites; it never
/// re-derives protocol policy.
pub trait WindowHost {
    /// A validated `Create` opened `window_id` with `surface` geometry,
    /// titled `title`, for the attested `owner`. `sizing` is what the app
    /// asks of the window manager's sizing: whether to present the window
    /// with a resize grabber and a live maximize/restore size toggle, and
    /// the smallest client size it may then be resized to. An error refuses
    /// the create: the engine unmaps the region and replies the refusal,
    /// keeping engine and host in lockstep.
    ///
    /// The host is the **enforcer** of a
    /// [`WindowSizing::Resizable`]'s declared minimum: it resizes no
    /// smaller than the larger of that and its own frame furniture's floor.
    /// The app states its minimum here once and lays out at whatever size
    /// it is given, so nothing resizes itself back.
    ///
    /// `owner` is the kernel-attested caller, not anything the client
    /// said — it is the only trustworthy answer to "which application is
    /// this window?", so a host that shows the owning application's
    /// identity (its icon) resolves it from this and never from `title`.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host cannot open a window for (e.g. the
    /// desktop is tearing down); the refusal is relayed to the client.
    fn window_opened(
        &mut self,
        owner: ProcId,
        window_id: u64,
        surface: &DisplayMode,
        title: &str,
        sizing: WindowSizing,
    ) -> Result<(), Errno>;

    /// A validated `CreatePopup` opened undecorated popup `window_id` of
    /// `surface` geometry, owned by and stacked directly above the
    /// caller's own window `parent_window_id`, at `offset_x`/`offset_y`
    /// physical pixels from that parent's client origin. The host resolves
    /// the parent's current screen position, adds the offset, clamps the
    /// whole popup onto the screen, opens it **undecorated** (no title
    /// bar, no frame furniture — the popup is a transient, not a taskbar
    /// entry), and records the parent→popup link so the popup stays glued
    /// above its parent and is torn down with it.
    ///
    /// An error refuses the popup: the engine unmaps the region and
    /// replies the refusal, keeping engine and host in lockstep exactly as
    /// [`window_opened`](Self::window_opened) does. The default is an
    /// infallible no-op: a host with no compositor to stack a popup on
    /// (a test double) accepts it without drawing anything.
    fn popup_opened(
        &mut self,
        window_id: u64,
        parent_window_id: u64,
        offset_x: i32,
        offset_y: i32,
        surface: &DisplayMode,
    ) -> Result<(), Errno> {
        let _ = (window_id, parent_window_id, offset_x, offset_y, surface);
        Ok(())
    }

    /// A validated `CreateTool` opened tool window `window_id`, a transient
    /// of the caller's own top-level window `spec.transient.parent_window_id`
    /// that the host frames with a mini title band reading `spec.title` and
    /// the user moves. The host places it as it places a popup — or, when
    /// `spec.carry` is set and the parent holds the seat's primary press,
    /// under the pointer, that press carried on as a move of the new window —
    /// and stacks it with its parent, off the taskbar, without the keyboard.
    /// A `spec.sizing` the user may resize it within gives it resize edges,
    /// never a maximize, the host enforcing the range as a window's; each
    /// size a drag settles is delivered as a [`WindowEvent::Resized`] on the
    /// tool window's own id.
    ///
    /// An error refuses it exactly as [`popup_opened`](Self::popup_opened)'s
    /// does, and the default accepts it without drawing anything.
    fn tool_opened(&mut self, window_id: u64, spec: &ToolSpec) -> Result<(), Errno> {
        let _ = (window_id, spec);
        Ok(())
    }

    /// A validated `CreateDialog` opened dialog window `window_id`, a
    /// transient of the caller's own top-level window
    /// `spec.transient.parent_window_id`. The host frames it as a dialog
    /// whose band reads `spec.title` after the name it attests the owner's
    /// application by, centres it over its owner on screen, stacks it with
    /// its owner off the taskbar, gives it the keyboard only when `focus` —
    /// the caller has earned the front — and, for
    /// [`DialogModality::OwnerModal`], holds the owner until it closes.
    ///
    /// An error refuses it exactly as [`popup_opened`](Self::popup_opened)'s
    /// does, and the default accepts it without drawing anything.
    fn dialog_opened(
        &mut self,
        window_id: u64,
        focus: bool,
        spec: &DialogSpec,
    ) -> Result<(), Errno> {
        let _ = (window_id, focus, spec);
        Ok(())
    }

    /// A validated, capability-gated `OpenLayer` opened desktop layer
    /// surface `window_id` of `surface` geometry for the attested `owner`,
    /// at screen point `x`/`y` in stacking layer `depth`.
    ///
    /// The engine has checked the caller's kernel-attested
    /// `CAP_DESKTOP_LAYER`, the geometry, and the per-client and per-seat
    /// surface counts. The host owns what is left, because only it knows
    /// the screen: clamping the surface onto the work area, re-checking the
    /// extent against the **live** UI scale (the wire bound is the ceiling
    /// at a scale of one), stacking it under the session's own chrome,
    /// keeping it out of the focus rotation and off the taskbar, and
    /// hit-testing it against its own content alpha.
    ///
    /// Unlike [`popup_opened`](Self::popup_opened) the default **refuses**:
    /// this is privileged authority, and a host that has not implemented it
    /// must not appear to grant it.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host refuses the surface with; the engine unmaps
    /// the region and relays the refusal, so engine and host stay in
    /// lockstep.
    fn layer_opened(
        &mut self,
        owner: ProcId,
        window_id: u64,
        surface: &DisplayMode,
        x: i32,
        y: i32,
        depth: LayerDepth,
    ) -> Result<(), Errno> {
        let _ = (owner, window_id, surface, x, y, depth);
        Err(Errno::NotSupported)
    }

    /// The engine refused a layer operation from `owner` at its capability
    /// gate, before any state was touched.
    ///
    /// Reported because the gate is enforced *here* rather than by the
    /// kernel — a session cannot bind a restricted-sender endpoint — so
    /// without this the most security-relevant refusal on the channel would
    /// be the one nothing recorded. The default is a no-op: a host with no
    /// audit log to write to is not obliged to invent one.
    fn layer_refused(&mut self, owner: ProcId, reason: Errno) {
        let _ = (owner, reason);
    }

    /// A validated `PlaceLayer` moved layer surface `window_id` to screen
    /// point `x`/`y` in stacking layer `depth`. Clamping is the host's, as
    /// at open.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host refuses the move with.
    fn layer_placed(
        &mut self,
        window_id: u64,
        x: i32,
        y: i32,
        depth: LayerDepth,
    ) -> Result<(), Errno> {
        let _ = (window_id, x, y, depth);
        Err(Errno::NotSupported)
    }

    /// Fill `out` with the visible windows' screen rectangles, back-to-front,
    /// as layer surface `window_id` sees them, and return how many were
    /// written.
    ///
    /// The asking surface is never its own terrain. A desktop with more
    /// visible windows than `out` holds is reported truncated to the
    /// frontmost, which are the ones a surface can actually meet.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host refuses the query with.
    fn layer_terrain(&mut self, window_id: u64, out: &mut [TerrainPlate]) -> Result<usize, Errno> {
        let _ = (window_id, out);
        Err(Errno::NotSupported)
    }

    /// A validated `Resize` re-mapped live `window_id` onto a fresh frame
    /// region of `surface` geometry, keeping the same window id, owner,
    /// event route, and taskbar entry. The host resizes the window's
    /// presented surface to match; the next `Present` shapes its frame
    /// against the new `surface`. An error refuses the resize: the engine
    /// keeps the previous mapping and replies the refusal, so engine and
    /// host never disagree about a window's geometry.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host cannot resize a window for (a surface it
    /// cannot reallocate, the desktop is tearing down); the refusal is
    /// relayed to the client and the old geometry stands.
    fn window_resized(&mut self, window_id: u64, surface: &DisplayMode) -> Result<(), Errno>;

    /// A validated `SetTitle`: the attested owner of live `window_id`
    /// retitled it to `title`, already bounded and control-character-free
    /// by the engine's ABI decode. The host applies it to the window's
    /// chrome and its taskbar entry from this one call, so the two can
    /// never disagree.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host cannot retitle for (it is tearing down, its
    /// compositor no longer holds the window); the refusal is relayed to
    /// the client and the previous title stands.
    fn window_retitled(&mut self, window_id: u64, title: &str) -> Result<(), Errno>;

    /// A validated `SetSizing`: the attested owner of live `window_id` — a
    /// window or a tool window — restated the range a *user* may resize it
    /// within. The host adopts it
    /// as the range it enforces from here on, in place of the one the
    /// `Create` declared.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host cannot adopt the range for — it is tearing
    /// down, its compositor no longer holds the window, or the sizing
    /// contradicts how the window was decorated (a resizable window handed
    /// a fixed sizing, or the reverse: what may change is the range, not
    /// whether the window has a grabber at all). The refusal is relayed to
    /// the client and the previous range stands.
    fn window_sizing_changed(&mut self, window_id: u64, sizing: WindowSizing) -> Result<(), Errno>;

    /// A validated `SetSizeState`: the attested owner of live `window_id`
    /// asked for it to be put into `state`. The host decides, and reports
    /// the state it actually applied — with the resulting client extent —
    /// as a `Resized` event.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host cannot apply the state for — it is tearing
    /// down, its compositor no longer holds the window, or the window
    /// cannot take the state at all (a fixed-size window has no state but
    /// the one it was created at). The refusal is relayed to the client
    /// and the window stays where it is.
    fn window_size_state_changed(
        &mut self,
        window_id: u64,
        state: WindowSizeState,
    ) -> Result<(), Errno>;

    /// `window_id` is gone — closed by its owner or torn down after the
    /// owner exited. Infallible: the window is already unmapped and
    /// forgotten by the engine, and the host must not resurrect it.
    fn window_closed(&mut self, window_id: u64);

    /// A validated `PickFile`: the attested owner of live window
    /// `window_id` (which has no pick pending, its application within
    /// [`PICK_MAX_OPEN`](tairix_abi::window_ipc::PICK_MAX_OPEN)) asked for the
    /// session's trusted file picker to choose a file for `purpose`. The
    /// picker takes the keyboard only when `focus` — the application holds
    /// the front, as an alert's must — and otherwise opens beneath the window
    /// that has it. The host opens its picker UI and, when the user concludes,
    /// routes the outcome back through [`WindowServer::conclude_pick`] — the
    /// engine tracks the pending pick and enforces that exactly one conclusion
    /// follows each acceptance.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host cannot run a picker for (a lock or a system
    /// prompt holds the seat, it shows as many picks as it may, it holds no
    /// filesystem authority, the desktop is tearing down); the refusal is
    /// relayed to the client and no pick is recorded.
    fn pick_requested(
        &mut self,
        window_id: u64,
        purpose: &PickPurpose,
        focus: bool,
    ) -> Result<(), Errno>;

    /// `caller` asked for a pick on its window `window_id` past a bound on
    /// the picks open, and was refused for `reason`: a decision for the
    /// host's audit trail.
    fn pick_refused(&mut self, caller: ProcId, window_id: u64, reason: &'static str) {
        let _ = (caller, window_id, reason);
    }

    /// A validated `BeginDrag`: the attested owner of live window
    /// `window_id` (which has no drag pending) began dragging `items`. The
    /// host takes the gesture over, reports where it goes through
    /// [`WindowServer::report_drag`], and when it ends routes the outcome back
    /// through [`WindowServer::conclude_drag`].
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host cannot carry the drag for — the press that
    /// began it is not the window's, or the seat is held by something a drag
    /// may not displace. Nothing is recorded.
    fn drag_requested(&mut self, _window_id: u64, _items: &DragItems) -> Result<(), Errno> {
        Err(Errno::NotSupported)
    }

    /// A validated `DragVerdict`: the owner of window `window_id`, whose drag
    /// is still carried, answered the report numbered `serial` with
    /// `verdict`. The host decides whether the answer is still current.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host refuses the answer with; nothing changes.
    fn drag_verdict(
        &mut self,
        _window_id: u64,
        _serial: u32,
        _verdict: Option<DropOperation>,
    ) -> Result<(), Errno> {
        Err(Errno::NotSupported)
    }

    /// A validated `DropLanding`: the owner of window `window_id` took `step`
    /// of the drop numbered `serial` its drag concluded on the desktop. A
    /// window going ends every drop it was landing here, before
    /// [`window_closed`](Self::window_closed); one a later drop displaced is
    /// the serial [`WindowServer::conclude_drag`] answers.
    fn drop_landing(&mut self, _window_id: u64, _serial: u32, _step: &LandingStep) {}

    /// A validated `SaveOfferAnswer`: the owner of window `window_id`
    /// answered the save offer numbered `serial` with whether a drop there
    /// saves. The host decides whether the answer is still current.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host refuses the answer with; nothing changes.
    fn save_offer_answered(
        &mut self,
        _window_id: u64,
        _serial: u32,
        _accepts: bool,
    ) -> Result<(), Errno> {
        Err(Errno::NotSupported)
    }

    /// A validated `ItemsOfferAnswer`: the owner of window `window_id`
    /// answered the desktop's items offer numbered `serial` with what a drop
    /// there would do. The host decides whether the answer is still current.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host refuses the answer with; nothing changes.
    fn items_offer_answered(
        &mut self,
        _window_id: u64,
        _serial: u32,
        _verdict: Option<DropOperation>,
    ) -> Result<(), Errno> {
        Err(Errno::NotSupported)
    }

    /// A validated `FileOfferAnswer`: the owner of window `window_id` answered
    /// the file offer numbered `serial` with whether a drop there places it.
    /// The host decides whether the answer is still current.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host refuses the answer with; nothing changes.
    fn file_offer_answered(
        &mut self,
        _window_id: u64,
        _serial: u32,
        _places: bool,
    ) -> Result<(), Errno> {
        Err(Errno::NotSupported)
    }

    /// The media type of the one file the offer numbered `serial` holds over
    /// window `window_id`, for its owner: a validated `OfferedFile` reads it.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] where that offer no longer stands over the window.
    fn offered_file(&self, _window_id: u64, _serial: u32) -> Result<&str, Errno> {
        Err(Errno::NotFound)
    }

    /// How many items the desktop's drag numbered `drag` carries, for the
    /// owner of window `window_id`, which it is offered to or dropped on: a
    /// validated `OfferedItems` reads them.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] where that drag has neither an offer standing over
    /// the window nor a drop on it.
    fn offered_count(&self, _window_id: u64, _drag: u32) -> Result<u32, Errno> {
        Err(Errno::NotFound)
    }

    /// Item `index` of the desktop's drag numbered `drag`, for the owner of
    /// window `window_id` ([`offered_count`](Self::offered_count)): its
    /// absolute path and the node listed there.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] past the last item, or as
    /// [`offered_count`](Self::offered_count).
    fn offered_item(
        &self,
        _window_id: u64,
        _drag: u32,
        _index: u32,
    ) -> Result<OfferedItem<'_>, Errno> {
        Err(Errno::NotFound)
    }

    /// A decoded `SaveInto`: attested `caller` answered the drop numbered
    /// `serial` on window `window_id` with `location`, a `fd_grant_location`
    /// handle it minted to the session, or zero to decline.
    ///
    /// The window is not yet known to be `caller`'s: the host acts only on
    /// the drop it awaits from that window's owner, learns the folder by
    /// redeeming `location` bound to `caller`, and consumes a non-zero
    /// `location` whatever it answers, so a refused one leaves nothing
    /// pending for the caller's life.
    ///
    /// The default refuses: a host that offers no saves awaits none.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host refuses the answer with.
    fn save_into(
        &mut self,
        caller: ProcId,
        window_id: u64,
        serial: u32,
        location: u64,
    ) -> Result<(), Errno> {
        let _ = (caller, window_id, serial, location);
        Err(Errno::NotSupported)
    }

    /// A validated `HandOverLaunch`: attested `caller` asked the session to
    /// reach the live instance of the bundle whose entry binary is
    /// `run_path`, handing it `target`.
    ///
    /// The host owns the decision, because only it knows which bundles are
    /// running and what their manifests attest: it resolves the launch
    /// through its own funnel and answers [`HandOverOutcome::NotRunning`]
    /// when there was no instance to reach, which is what tells the caller to
    /// launch the bundle itself. `desk` is the one capability the *engine*
    /// owns and lends for the occasion — queueing a target, waking its owner,
    /// and spending the activation a gesture lent the caller — so the launch
    /// rule stays in one place rather than being re-derived here.
    ///
    /// A document's grant is one `caller` minted **to the session**, from a
    /// descriptor the caller opened under its own authority. The host redeems
    /// it only as `caller`'s own — bound to its attested instance, so a caller
    /// naming a delegation somebody else minted to the session gets nothing —
    /// and hands it on to the instance it resolved; it never opens a path on a
    /// caller's behalf, which would lend the session's own larger reach.
    ///
    /// The default refuses: a host with no launch table cannot say whether
    /// anything is running, and telling the caller so is more honest than
    /// answering "not running" for a bundle it never looked for.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host could not act on the request with — a grant it
    /// could not redeem, a re-grant the kernel refused. Nothing is delegated
    /// on a refusal.
    fn hand_over_requested(
        &mut self,
        desk: &mut dyn HandOverDesk,
        caller: ProcId,
        run_path: &str,
        target: &HandOverTarget,
    ) -> Result<HandOverOutcome, Errno> {
        let _ = (desk, caller, run_path, target);
        Err(Errno::NotSupported)
    }

    /// A validated `PlaceDropped`: attested `caller` handed over `file` for the
    /// drop its drag placed, which `into` names — the placement and the
    /// application owning the window it was dropped on — or why nothing waits
    /// to be placed.
    ///
    /// The host relays the file read-only to that application, queued through
    /// `desk` as an [`OpenEntry::Placed`], and consumes the grant whatever it
    /// answers, so a refused hand-over leaves nothing pending for the caller's
    /// life.
    ///
    /// # Errors
    ///
    /// The refusal `into` carries, or any the relay met: a grant the caller did
    /// not mint, a descriptor that is not read-only, an application that no
    /// longer takes open targets.
    fn place_dropped(
        &mut self,
        desk: &mut dyn HandOverDesk,
        caller: ProcId,
        into: Result<(ProcId, Placement), Errno>,
        file: PlacedFile<'_>,
    ) -> Result<(), Errno> {
        let _ = (desk, caller, file);
        into.and(Err(Errno::NotSupported))
    }

    /// A validated `RevealDocument`: attested `caller` asked to have the user
    /// shown, in the file manager, the document behind its window
    /// `window_id`, telling where it is through `location` — a
    /// `fd_grant_location` handle it minted to the session.
    ///
    /// The host checks the window is the caller's and the one the user works
    /// in, learns the path by redeeming `location` bound to `caller`, and
    /// reaches the file manager through `desk` exactly as its own launches
    /// do. It consumes `location` whatever it answers, so a refused reveal
    /// leaves nothing pending for the caller's life.
    ///
    /// The default refuses: a host with no file manager to reach shows
    /// nothing, and saying so beats pretending it did.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host could not show the document with.
    fn reveal_requested(
        &mut self,
        desk: &mut dyn HandOverDesk,
        caller: ProcId,
        window_id: u64,
        location: u64,
    ) -> Result<(), Errno> {
        let _ = (desk, caller, window_id, location);
        Err(Errno::NotSupported)
    }

    /// A validated `OpenMenu`: the attested owner of live window
    /// `window_id` (which has no open unanswered) asked for a menu chain,
    /// anchored at `anchor` in that window's own client pixels, over
    /// `menu`'s rows — already bounded and shape-checked by the engine's
    /// ABI decode, and carrying its root plate's title.
    ///
    /// The host places, draws, grabs and routes the chain, and when it
    /// closes routes the answer back through
    /// [`WindowServer::deliver_event`] as a `MenuClosed` naming `open_id`
    /// — the engine tracks the open and enforces that exactly one outcome
    /// follows each acceptance.
    ///
    /// Accepting must not depend on the requesting application: nothing
    /// about bringing a chain up may wait on a client.
    ///
    /// The default refuses: a host that composes no menu service cannot
    /// honour a chain, and telling the application so is more honest than
    /// accepting an open nothing will ever answer.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host cannot open a chain for; the refusal is
    /// relayed to the client, which reports it and carries on, and no open
    /// is recorded.
    fn menu_open_requested(
        &mut self,
        window_id: u64,
        open_id: u64,
        anchor: WindowRegion,
        menu: &AppMenu,
    ) -> Result<(), Errno> {
        let _ = (window_id, open_id, anchor, menu);
        Err(Errno::NotSupported)
    }

    /// A validated `RestateMenu`: the attested owner of live window
    /// `window_id`, whose open `open_id` is still unanswered, asked for that
    /// chain to show `menu` from now on — bounded and shape-checked by the
    /// ABI decode exactly as an open's — answering every row chosen from it
    /// with `revision`. A chain that ended while the request was on its way
    /// has its answer queued, so the host restates nothing and succeeds.
    ///
    /// The default refuses, as [`Self::menu_open_requested`] does.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host cannot restate the chain for; the chain is left
    /// as it was.
    fn menu_restate_requested(
        &mut self,
        window_id: u64,
        open_id: u64,
        revision: u32,
        menu: &AppMenu,
    ) -> Result<(), Errno> {
        let _ = (window_id, open_id, revision, menu);
        Err(Errno::NotSupported)
    }

    /// A validated `OpenAlert`: put `open` up in a window of its own, and
    /// answer it later through [`WindowServer::answer_alert`] — exactly once,
    /// unless the engine withdraws it first ([`alert_withdrawn`]).
    ///
    /// The default refuses: a host with no desktop to draw on cannot honour
    /// an alert, and saying so lets the application state its message
    /// another way.
    ///
    /// [`alert_withdrawn`]: Self::alert_withdrawn
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host cannot put the alert up for; nothing is
    /// recorded and no id is spent.
    fn alert_opened(&mut self, open: &AlertOpen<'_>) -> Result<(), Errno> {
        let _ = open;
        Err(Errno::NotSupported)
    }

    /// Take alert `alert_id` down: the engine has already answered it
    /// (withdrawn, or its owner window closed) or its application is gone.
    fn alert_withdrawn(&mut self, alert_id: u64) {
        let _ = alert_id;
    }

    /// A validated `SetTooltip`: the attested owner of live window
    /// `window_id` declared that `region` of its own client pixels is
    /// explained by `text`, or — with `text` empty — withdrew whatever it
    /// had declared.
    ///
    /// Everything a tooltip *does* is the host's: the dwell before it
    /// appears, where the plate goes so it stays on screen, and every
    /// reason it comes down again. The engine only validates the window and
    /// the bounds and relays the declaration.
    ///
    /// The default refuses: a host with no seat to hover on cannot honour a
    /// tip, and saying so is more honest than accepting one nothing draws.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host cannot hold a declaration for; the refusal is
    /// relayed to the client, which reports it and carries on.
    fn tooltip_declared(
        &mut self,
        window_id: u64,
        region: WindowRegion,
        text: &str,
    ) -> Result<(), Errno> {
        let _ = (window_id, region, text);
        Err(Errno::NotSupported)
    }

    /// The caller's own window `window_id` asked for the pointer to show
    /// `shape` over its client area.
    ///
    /// The default refuses: a host with no pointer to draw has nothing to set.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host cannot honour the shape with.
    fn cursor_set(&mut self, window_id: u64, shape: CursorShape) -> Result<(), Errno> {
        let _ = (window_id, shape);
        Err(Errno::NotSupported)
    }

    /// The caller's own window `window_id` asked to read the keyboard as
    /// `mode` while it holds it.
    ///
    /// The default refuses: a host with no keyboard reads none.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host cannot honour the mode with.
    fn keyboard_mode_set(&mut self, window_id: u64, mode: KeyboardMode) -> Result<(), Errno> {
        let _ = (window_id, mode);
        Err(Errno::NotSupported)
    }

    /// The caller's own window `window_id` asked for the pointer to be locked
    /// to it (`locked`) or let go; a granted lock and every end of one is told
    /// to the window as [`WindowEvent::PointerLock`].
    ///
    /// The default refuses: a host with no pointer locks none.
    ///
    /// # Errors
    ///
    /// [`Errno::PermissionDenied`] when the window may not lock the pointer
    /// now, or any [`Errno`] the host cannot honour the request with.
    fn pointer_lock_set(&mut self, window_id: u64, locked: bool) -> Result<(), Errno> {
        let _ = (window_id, locked);
        Err(Errno::NotSupported)
    }

    /// The window holding the keyboard, if one of this engine's windows does.
    ///
    /// The default answers none: a host with no keyboard focuses nothing.
    fn focused_window(&self) -> Option<u64> {
        None
    }

    /// Raise window `window_id` and give it the keyboard. The engine asks only
    /// once the caller owns the window and holds an activation or the
    /// keyboard ([`WindowRequest::ActivateWindow`]).
    ///
    /// The default refuses: a host that stacks no windows has nothing to raise.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host cannot raise the window with: a seat busy with
    /// a lock or a carried drag, a window not on screen.
    fn raise_requested(&mut self, window_id: u64) -> Result<(), Errno> {
        let _ = window_id;
        Err(Errno::NotSupported)
    }

    /// `caller` asked to raise its window `window_id` holding neither an
    /// activation the keyboard has not moved away from nor the keyboard, and
    /// was refused: a decision for the host's audit trail.
    fn raise_refused(&mut self, caller: ProcId, window_id: u64) {
        let _ = (caller, window_id);
    }

    /// The caller's own window `window_id` asked to put the first `len`
    /// bytes of `region` on the clipboard as `kind`.
    ///
    /// The default refuses: a host with no clipboard has nowhere to put it.
    ///
    /// # Errors
    ///
    /// The host's refusal: a window the user is not working in, a region the
    /// caller did not grant or shorter than `len`, text that is not.
    fn clipboard_set(
        &mut self,
        window_id: u64,
        region: ClientRegion,
        len: u64,
        kind: ClipboardKind,
    ) -> Result<(), Errno> {
        let _ = (window_id, region, len, kind);
        Err(Errno::NotSupported)
    }

    /// The caller's own window `window_id` asked for the clipboard, copied
    /// into `region`.
    ///
    /// # Errors
    ///
    /// The host's refusal, as for [`clipboard_set`](Self::clipboard_set).
    fn clipboard_get(
        &mut self,
        window_id: u64,
        region: ClientRegion,
    ) -> Result<ClipboardHeld, Errno> {
        let _ = (window_id, region);
        Err(Errno::NotSupported)
    }

    /// A validated `SetAppBar`: the attested `owner` declared (or
    /// re-declared) its presence on the desktop's icon bar — its event
    /// route, whether it handles the primary click, and its menu, all
    /// already bounded and shape-checked by the engine's ABI decode.
    ///
    /// A re-declaration replaces the previous one whole; that is how an
    /// application changes a row's enablement or its mark.
    ///
    /// The default refuses: a host that composes no icon bar cannot honour
    /// a slot, and telling the application so is more honest than
    /// accepting a declaration nothing will ever draw.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host cannot list the application under; the
    /// refusal is relayed to the client, which reports it and carries on.
    fn app_bar_declared(&mut self, owner: ProcId, bar: &AppBar) -> Result<(), Errno> {
        let _ = (owner, bar);
        Err(Errno::NotSupported)
    }

    /// The attested `owner` is gone; drop the icon-bar presence it
    /// declared, if any. Infallible: the engine has already forgotten the
    /// route, and the host must not resurrect the slot.
    ///
    /// Called from [`WindowServer::client_exited`] only for a client that
    /// actually held a declaration, so a host need not filter.
    fn app_bar_withdrawn(&mut self, owner: ProcId) {
        let _ = owner;
    }

    /// A validated `SetBackdropBlur`: the attested owner of live `window`
    /// set its backdrop-blur radius to `radius_px` logical pixels, already
    /// bounded by the engine's ABI decode to `WINDOW_BACKDROP_BLUR_MAX_PX`.
    /// Infallible: it only changes how the host recomposites the window's
    /// own rectangle, never another principal's state, so there is nothing
    /// for a host to refuse; the default does nothing.
    fn backdrop_blur_set(&mut self, window: u64, radius_px: u16) {
        let _ = (window, radius_px);
    }

    /// Describe the desktop this host composites: the screen extent, the
    /// UI scale, and the active appearance.
    ///
    /// The host is the authority for all three — it owns the compositor
    /// and the theme registry — so the answer is read straight from it
    /// rather than cached in the engine, and an application can never see
    /// a desktop the session has already left behind.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host has no desktop to describe (it is tearing
    /// down, or its screen extent is not one the record admits); the
    /// refusal is relayed to the client, which then knows it does not
    /// know rather than drawing to a guess.
    fn desktop(&mut self) -> Result<DesktopInfo, Errno>;

    /// The shipped wallpaper catalog this host offers, in catalog order.
    ///
    /// The host is the authority because it is the only party that may
    /// read the store; it lists that store once — `/System` is read-only,
    /// so the catalog is fixed for the life of the boot — and holds the
    /// result, so answering a query costs no I/O on the serve loop.
    ///
    /// The default is empty: a host that has listed no store has none to
    /// describe, which is the honest "this desktop offers no shipped
    /// pictures" rather than a refusal.
    fn wallpaper_catalog(&mut self) -> &[WallpaperName] {
        &[]
    }

    /// The cursor sets this host offers, in the order a chooser lists
    /// them.
    ///
    /// The host is the authority because it is the only party that may read
    /// the store; it lists that store once at bring-up — `/System` is
    /// read-only, so the choice space is fixed for the life of the boot —
    /// and holds the result, so answering a query costs no I/O on the serve
    /// loop.
    ///
    /// The default is empty: a host that has listed no store offers no sets
    /// of its own, which is the honest answer rather than a refusal.
    fn cursor_sets(&mut self) -> &[CursorSetName] {
        &[]
    }

    /// A validated `RenderPreview`: render `subject` as a `width`x`height`
    /// straight-alpha RGBA8 picture into `region`, concluding to `window_id`.
    ///
    /// The engine has checked that the caller owns the window, that the same
    /// picture is not already pending on it at this size, and that the size is
    /// within the ABI bound. The host owns what is left, because only it holds
    /// the stores and the parser sandbox: bounding how many renders pend,
    /// resolving the subject against what it knows itself, mapping `region`
    /// from its grantor and checking it holds `width * height * 4` bytes,
    /// and doing the read and the decode **off** its compositing loop. It
    /// concludes by delivering exactly one [`WindowEvent::PreviewRendered`]
    /// naming the same subject and size.
    ///
    /// The default refuses: a host with no store to read cannot render a
    /// picture, and saying so is more honest than accepting a request that
    /// would never conclude.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the host refuses the render with — a subject it does not
    /// hold ([`Errno::NotFound`]), a region too small or ungranted
    /// ([`Errno::LengthOutOfRange`], [`Errno::NotFound`]). A refusal leaves no
    /// render pending, so the caller may ask again.
    ///
    /// [`WindowEvent::PreviewRendered`]: tairix_abi::window_ipc::WindowEvent::PreviewRendered
    fn preview_render_requested(
        &mut self,
        caller: Option<&AppIdentity>,
        window_id: u64,
        region: ClientRegion,
        request: PreviewSize,
    ) -> Result<(), Errno> {
        let _ = (caller, window_id, region, request);
        Err(Errno::NotSupported)
    }

    /// The sources that have posted a notification since the desktop started,
    /// answered to `caller`, the application the kernel attests is asking.
    ///
    /// The host decides who may learn this; the default refuses, because a
    /// host that keeps no such record has nothing honest to answer.
    ///
    /// # Errors
    ///
    /// [`Errno::PermissionDenied`] for a caller the host does not answer, or
    /// [`Errno::NotSupported`] from a host that keeps no record.
    fn notify_sources(&mut self, caller: Option<&AppIdentity>) -> Result<&[BundleId], Errno> {
        let _ = caller;
        Err(Errno::NotSupported)
    }

    /// Lock the screen now, at the request of `caller`, the application the
    /// kernel attests is asking.
    ///
    /// The host decides who may ask; the default refuses, because a host with
    /// no lock cannot honour one.
    ///
    /// # Errors
    ///
    /// [`Errno::PermissionDenied`] for a caller the host does not honour, or
    /// the host's own refusal to lock.
    fn lock_screen(&mut self, caller: Option<&AppIdentity>) -> Result<(), Errno> {
        let _ = caller;
        Err(Errno::NotSupported)
    }

    /// Write entry `index` of the screensaver catalogue into `out` at the
    /// request of `caller`, the application the kernel attests is asking,
    /// answering the bytes written (`encode_screensaver_reply`).
    ///
    /// The host decides who may ask: which savers a user installed is theirs.
    /// The default refuses, because a host with no catalogue has none to
    /// describe.
    fn screensaver_entry(
        &mut self,
        caller: Option<&AppIdentity>,
        index: u16,
        out: &mut [u8; WINDOW_SCREENSAVER_REPLY_MAX],
    ) -> usize {
        let _ = (caller, index);
        encode_screensaver_reply(out, Err(Errno::NotSupported))
    }

    /// Show the screensaver `document` describes now, as a preview, at the
    /// request of `caller`, the application the kernel attests is asking.
    ///
    /// The host decides who may ask and reads the document; the default
    /// refuses, because a host with no screensaver cannot show one.
    ///
    /// # Errors
    ///
    /// [`Errno::PermissionDenied`] for a caller the host does not honour,
    /// [`Errno::OutOfRange`] for a document it will not read,
    /// [`Errno::SeatBusy`] while the screen is not the desktop's to show one
    /// on, and [`Errno::NotSupported`] from a host with no screensaver.
    fn screensaver_preview_requested(
        &mut self,
        caller: Option<&AppIdentity>,
        document: &str,
    ) -> Result<(), Errno> {
        let _ = (caller, document);
        Err(Errno::NotSupported)
    }

    /// A read-only delegation of the shipped civil-time store to the process
    /// `to`, at the request of `caller`, the application the kernel attests
    /// is asking: the `fd_grant` handle minted to `to`.
    ///
    /// The host decides who may ask; the default refuses, because a host that
    /// holds no store has none to hand on.
    ///
    /// # Errors
    ///
    /// [`Errno::PermissionDenied`] for a caller the host does not answer,
    /// [`Errno::NotFound`] when it holds no store, the kernel's refusal of
    /// the delegation, and [`Errno::NotSupported`] from a host with no store
    /// to hand on.
    fn zone_store(&mut self, to: ProcId, caller: Option<&AppIdentity>) -> Result<u64, Errno> {
        let _ = (to, caller);
        Err(Errno::NotSupported)
    }

    /// Put the system's authorisation prompt up for `task`, asked by
    /// `caller`, the application the kernel attests is asking, for `asker`:
    /// its own window, or its icon-bar slot.
    ///
    /// The engine has checked the window is the caller's, or the slot one it
    /// declared, and that no authorisation is pending there; who may ask, what
    /// the task runs and what the prompt says are the host's. Its outcome is
    /// answered through [`WindowServer::conclude_authorisation`]. The default
    /// refuses, because a host with no prompt cannot ask.
    ///
    /// # Errors
    ///
    /// [`Errno::PermissionDenied`] for a caller the host does not honour,
    /// [`Errno::OutOfRange`] for a task it will not perform as asked,
    /// [`Errno::SeatBusy`] while anything else holds the seat, and
    /// [`Errno::NotSupported`] from a host with no prompt.
    fn authorise_requested(
        &mut self,
        caller: Option<&AppIdentity>,
        asker: AuthoriseFor,
        task: &SystemTask,
    ) -> Result<(), Errno> {
        let _ = (caller, asker, task);
        Err(Errno::NotSupported)
    }
}

/// Who an authorisation was asked for, and so where its answer goes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AuthoriseFor {
    /// The asking application's own window.
    Window(u64),
    /// The icon-bar slot of the application that is this process, which has
    /// no window to hold the question.
    Slot(ProcId),
}

/// An icon-bar slot's authorisation: pending until it concludes, then what
/// a captured task printed until the application takes it.
#[derive(Default)]
struct SlotAuthorisation {
    pending: bool,
    output: Option<Vec<u8>>,
}

/// Where [`WindowServer::latch`] shows each window's newest presented frame:
/// the compositor's side of a present, reached once a desktop frame rather
/// than once a request.
pub trait FrameHost {
    /// Show `damage` of `frame`, window `window_id`'s newest presented frame,
    /// exactly one frame of its region shaped as `surface`.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] compositing fails with; the engine hands it to
    /// [`Self::frame_refused`].
    fn window_presented(
        &mut self,
        window_id: u64,
        surface: &DisplayMode,
        frame: &[u8],
        damage: &DamageList,
    ) -> Result<(), Errno>;

    /// Window `window_id`'s frame could not be shown, the first refused since
    /// the window last showed one. The window keeps the pixels it had, and the
    /// frame is tried again with the next frame the desktop composes, carrying
    /// what a later present changes as well.
    fn frame_refused(&mut self, window_id: u64, errno: Errno);
}

/// The newest frame a window presented that the session has not shown, and
/// everything changed since the last one it showed.
///
/// One per window however often it presents: a present while one is pending
/// replaces the frame and adds its change, which is sound because a client
/// brings each frame up to date before presenting it.
struct Pending {
    frame: Option<u32>,
    damage: Region,
    /// The frame named was refused, so it waits for a frame the desktop
    /// composes rather than asking for one: a lasting refusal would otherwise
    /// compose a frame every period for nothing.
    refused: bool,
    /// A frame was refused since the window last showed one, and the host was
    /// told.
    reported: bool,
    /// The ticket of the present that replaced an unshown frame, its answer
    /// held until the frame it named is shown or refused, or something
    /// supersedes it.
    withheld: Option<u64>,
}

impl Pending {
    const fn new() -> Self {
        Self {
            frame: None,
            damage: Region::with_budget(MAX_DAMAGE_RECTS),
            refused: false,
            reported: false,
            withheld: None,
        }
    }

    /// Frame `frame_index` is the newest, `damage` of it changed.
    fn record(&mut self, frame_index: u32, damage: &DamageList, surface: &DisplayMode) {
        self.frame = Some(frame_index);
        self.refused = false;
        let bounds = Rect::new(0, 0, surface.width_px, surface.height_px);
        add_damage(&mut self.damage, damage.rects(), bounds);
    }

    /// Nothing is pending: the frames it named are gone.
    fn clear(&mut self) {
        self.frame = None;
        self.refused = false;
        self.damage.clear();
    }

    /// The frame named was shown.
    fn shown(&mut self) {
        self.clear();
        self.reported = false;
    }

    /// The frame named could not be shown. Answers whether the host is to be
    /// told, which it is once until the window next shows a frame.
    fn refuse(&mut self) -> bool {
        self.refused = true;
        !core::mem::replace(&mut self.reported, true)
    }
}

/// How many windows hold a frame [`WindowServer::latch`] has yet to show, kept
/// as each changes so asking costs no walk of every window.
#[derive(Clone, Copy)]
struct Unshown {
    /// Frames that ask the desktop for a frame of their own.
    due: usize,
    /// Refused frames, which wait for one the desktop composes.
    refused: usize,
}

impl Unshown {
    const NONE: Self = Self { due: 0, refused: 0 };

    const fn is_empty(self) -> bool {
        self.due == 0 && self.refused == 0
    }

    /// Count `pending` in.
    fn count_in(&mut self, pending: &Pending) {
        match (pending.frame, pending.refused) {
            (None, _) => {}
            (Some(_), false) => self.due = self.due.saturating_add(1),
            (Some(_), true) => self.refused = self.refused.saturating_add(1),
        }
    }

    /// Count `pending` out, before it changes or goes.
    fn count_out(&mut self, pending: &Pending) {
        match (pending.frame, pending.refused) {
            (None, _) => {}
            (Some(_), false) => self.due = self.due.saturating_sub(1),
            (Some(_), true) => self.refused = self.refused.saturating_sub(1),
        }
    }
}

/// One withheld present's answer, due.
#[derive(Copy, Clone)]
struct Answer {
    owner: ProcId,
    ticket: u64,
}

/// The answers to withheld presents: those due, oldest first, and how many
/// windows still hold one.
///
/// Room is kept for every held answer beside those due, so one falls due — at
/// a latch, a resize, a release or a close — without allocating.
struct Outbox {
    due: VecDeque<Answer>,
    held: usize,
}

impl Outbox {
    const fn new() -> Self {
        Self {
            due: VecDeque::new(),
            held: 0,
        }
    }

    /// Room for an answer from each of `windows` windows, and for the one a
    /// present replacing a held one sends at once.
    fn reserve(&mut self, windows: usize) -> Result<(), Errno> {
        let wanted = windows.saturating_add(1);
        self.due
            .try_reserve(wanted.saturating_sub(self.due.len()))
            .map_err(|_| Errno::OutOfMemory)
    }

    /// Hold `ticket`, `owner`'s present to the window `pending` belongs to,
    /// the answer it held before falling due now. Answers whether it is held:
    /// with no room kept for its answer it is not, and is answered at once.
    fn hold(&mut self, pending: &mut Pending, owner: ProcId, ticket: u64) -> bool {
        let room = self.due.capacity() - self.due.len() > self.held;
        self.release(pending, owner);
        if !room {
            return false;
        }
        pending.withheld = Some(ticket);
        self.held = self.held.saturating_add(1);
        true
    }

    /// The answer `pending`, a window of `owner`'s, holds falls due.
    fn release(&mut self, pending: &mut Pending, owner: ProcId) {
        if let Some(ticket) = pending.withheld.take() {
            self.held = self.held.saturating_sub(1);
            // Room was kept for every held answer, so this never allocates.
            self.due.push_back(Answer { owner, ticket });
        }
    }

    /// Forget the answer `pending` holds: its caller has gone.
    fn drop_held(&mut self, pending: &Pending) {
        if pending.withheld.is_some() {
            self.held = self.held.saturating_sub(1);
        }
    }

    /// Forget every answer due to `owner`, whose calls ended with it.
    fn forget(&mut self, owner: ProcId) {
        self.due.retain(|answer| answer.owner != owner);
    }

    /// Send every answer due through `reply`, oldest first: a reply lands in
    /// the room its call was posted with, so each is answered once sent.
    fn send(&mut self, mut reply: impl FnMut(u64, &[u8])) {
        let recorded = encode_status_reply(Ok(()));
        while let Some(answer) = self.due.pop_front() {
            reply(answer.ticket, &recorded);
        }
    }
}

/// What a present carries for `damage` of `surface`: its rectangles while they
/// fit one present's list, their bounds past that, and the whole surface for
/// damage that covers it.
fn damage_of(damage: &Region, surface: &DisplayMode) -> Result<DamageList, Errno> {
    let whole = [DamageRect::full(surface)];
    let mut out = [whole[0]; MAX_DAMAGE_RECTS];
    DamageList::new(damage_list(damage, surface, &mut out).unwrap_or(&whole))
}

/// What a `RenderPreview` names: the picture and the size it is rendered at,
/// gathered because the engine validated them together.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PreviewSize {
    /// The picture to render.
    pub subject: PreviewSubject,
    /// The width, in physical pixels.
    pub width: u16,
    /// The height, in physical pixels.
    pub height: u16,
}

/// The event-delivery seam — the session's app-ward send (`ipc_send` to
/// the owning app's event endpoint in production) behind a trait, so
/// event routing is host-testable.
pub trait EventSink {
    /// Deliver one [`WindowEvent`] to `endpoint`, encoding it on the way.
    ///
    /// The sink takes the typed event rather than its wire form because
    /// only the sink knows whether the event goes out now: one that holds
    /// an event back against a full mailbox folds it into what it already
    /// holds by *kind*, and encodes once, when it finally goes.
    ///
    /// # Errors
    ///
    /// Any [`Errno`] the delivery surfaces (a full queue, a dead
    /// receiver). The sink must never block the session on a slow
    /// receiver: it either accepts responsibility for the event or
    /// refuses it.
    fn deliver(&mut self, endpoint: u64, event: &WindowEvent) -> Result<(), Errno>;

    /// Whether a render conclusion for `window_id` is held undelivered for
    /// `endpoint`: a sink that holds nothing back holds none.
    ///
    /// Asked before a render is accepted, so a client that stops draining its
    /// mailbox cannot keep the sink holding conclusions on its behalf.
    fn holds_render(&self, endpoint: u64, window_id: u64) -> bool {
        let _ = (endpoint, window_id);
        false
    }
}

/// The four geometry fields a `Create`, `CreatePopup`, or `Resize` carries
/// on the wire, as the one surface value every spec holds.
const fn surface_of(
    width_px: u32,
    height_px: u32,
    stride_bytes: u32,
    format: DisplayFormat,
) -> DisplayMode {
    DisplayMode {
        width_px,
        height_px,
        stride_bytes,
        format,
    }
}

/// Everything one validated `Create` asks for, in one place, so the
/// engine's create path takes the request as a unit.
#[derive(Copy, Clone)]
struct CreateSpec {
    window_id: u64,
    shm_handle: u64,
    event_endpoint: u64,
    frame_count: u32,
    surface: DisplayMode,
    title: WindowTitle,
    sizing: WindowSizing,
}

/// Everything one validated `Resize` asks for, in one place, so the
/// engine's resize path takes the request as a unit.
#[derive(Copy, Clone)]
struct ResizeSpec {
    window_id: u64,
    shm_handle: u64,
    frame_count: u32,
    surface: DisplayMode,
}

/// Everything a transient — a popup, a tool window or a dialog — opens with,
/// in one place: the app half fills it in for the client's create call, and
/// the engine's transient path receives the decoded request as the same unit,
/// so both halves describe one once. Where it opens is each kind's own.
#[derive(Copy, Clone)]
pub struct TransientSpec {
    /// The caller's own top-level window the transient belongs to and is
    /// stacked above; a foreign, unknown, or transient parent is refused.
    pub parent_window_id: u64,
    /// The `shm_grant`ed region holding the transient's frames, mapped once.
    pub shm_handle: u64,
    /// The endpoint the transient's own events are delivered to.
    pub event_endpoint: u64,
    /// How many frames the region holds, back to back.
    pub frame_count: u32,
    /// The geometry of one frame, which must hold for every frame.
    pub surface: DisplayMode,
}

/// Everything one `CreatePopup` asks for: what any transient opens with, and
/// where from its parent's client origin.
#[derive(Copy, Clone)]
pub struct PopupSpec {
    /// The parent, region, event route and geometry.
    pub transient: TransientSpec,
    /// Physical pixels right of the parent window's client origin.
    pub offset_x: i32,
    /// Physical pixels below the parent window's client origin.
    pub offset_y: i32,
}

/// Everything one `CreateTool` asks for: what any transient opens with, where
/// from its parent's client origin, how it opens and what its band reads.
#[derive(Copy, Clone)]
pub struct ToolSpec {
    /// The parent, region, event route and geometry.
    pub transient: TransientSpec,
    /// Physical pixels right of the parent window's client origin.
    pub offset_x: i32,
    /// Physical pixels below the parent window's client origin.
    pub offset_y: i32,
    /// Where along its band, from the client's left edge, the press its
    /// parent holds carries it, if any; less than the client's width.
    pub carry: Option<u32>,
    /// What its mini title bar reads.
    pub title: WindowTitle,
    /// Whether the user may resize it, and within what range of clients.
    pub sizing: WindowSizing,
}

/// Everything one `CreateDialog` asks for: what any transient opens with,
/// whether it holds its owner and what its band reads. It names no place: the
/// host centres a dialog over its owner.
#[derive(Copy, Clone)]
pub struct DialogSpec {
    /// The owner, region, event route and geometry.
    pub transient: TransientSpec,
    /// Whether it holds its owner until it closes.
    pub modality: DialogModality,
    /// What its title band reads beside the application's name.
    pub title: WindowTitle,
}

/// A validated [`WindowRequest::OpenAlert`], as the host is asked to put it up.
pub struct AlertOpen<'a> {
    /// The id the engine minted; the host names it when the alert is answered.
    pub alert_id: u64,
    /// The kernel-attested caller, whose attested identity the host titles the
    /// alert with.
    pub owner: ProcId,
    /// The caller's own top-level window the alert holds, if it named one.
    pub window: Option<u64>,
    /// Whether the alert may take the keyboard and the front: its
    /// application holds the keyboard, or a live activation the user gave it
    /// (a drop, a hand-over), which accepting the alert spends.
    pub focus: bool,
    /// What the alert says.
    pub spec: &'a AlertSpec,
}

/// Everything a validated [`WindowRequest::OpenLayer`] carries.
pub struct LayerSpec {
    /// The `shm_grant`ed region holding the surface's frames, mapped once.
    pub shm_handle: u64,
    /// The endpoint the surface's own events are delivered to.
    pub event_endpoint: u64,
    /// How many frames the region holds, back to back.
    pub frame_count: u32,
    /// The geometry of one frame, which must hold for every frame.
    pub surface: DisplayMode,
    /// Left edge in physical screen pixels, before the host clamps it.
    pub x: i32,
    /// Top edge in physical screen pixels, before the host clamps it.
    pub y: i32,
    /// Which desktop stacking layer the surface opens in.
    pub depth: LayerDepth,
}

/// The engine's own [`HandOverDesk`]: its queue and the sink the wake goes
/// out on, bound for the length of one hand-over.
struct EngineDesk<'a, M: ShmMapper> {
    server: &'a mut WindowServer<M>,
    sink: &'a mut dyn EventSink,
}

impl<M: ShmMapper> HandOverDesk for EngineDesk<'_, M> {
    fn hand_over(
        &mut self,
        app: ProcId,
        activation: Activation,
        make: &mut dyn FnMut() -> Result<OpenEntry, Errno>,
    ) -> bool {
        self.server
            .hand_over_open_target(self.sink, app, activation, make)
            .is_ok()
    }

    fn ask_default(&mut self, app: ProcId, activation: Activation) -> bool {
        self.server
            .deliver_app_event(self.sink, app, &WindowEvent::AppBarDefault, activation)
            .is_ok()
    }

    fn recent_window(&self, app: ProcId) -> Option<u64> {
        self.server
            .windows
            .iter()
            .rev()
            .find(|(_, record)| record.owner == app)
            .map(|(&id, _)| id)
    }

    fn spend_activation(&mut self, app: ProcId, focus: Option<u64>) -> bool {
        self.server.activations.remove(&app) == Some(focus)
    }
}

/// Whether an application event lets its receiver raise one of its own
/// windows ([`WindowRequest::ActivateWindow`]).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Activation {
    /// The event was not the user's doing, so it brings nothing forward.
    Withheld,
    /// One raise, valid while the keyboard is still at `focus` — the window
    /// holding it when the event was sent, `None` for none of the engine's.
    Granted {
        /// Where the keyboard was.
        focus: Option<u64>,
    },
}

/// The engine's half of reaching a live instance, lent to the host for the
/// length of one hand-over.
///
/// Whether to reach an instance is the host's decision — only it knows what
/// is running and what a manifest attests — while every *way* of reaching
/// one is a fact or an action the engine owns: the target queue and its
/// bound, the application's event route, which window it most recently
/// opened, and the activation a gesture lent the caller. So the engine hands
/// these over rather than either party re-deriving the other's half.
pub trait HandOverDesk {
    /// Queue the entry `make` produces for `app` and wake it, with what
    /// `activation` lets it bring forward, answering whether it was taken.
    ///
    /// `make` runs only once `app` is known to take the entry, since it may
    /// mint a delegation nothing can take back. `false` is an unreachable
    /// instance: nothing is left queued, so the caller is free to read it as
    /// "start a fresh process instead".
    fn hand_over(
        &mut self,
        app: ProcId,
        activation: Activation,
        make: &mut dyn FnMut() -> Result<OpenEntry, Errno>,
    ) -> bool;

    /// Ask `app` for its icon-bar default action, with what `activation` lets
    /// it bring forward. `false` when it declared no icon-bar presence, so it
    /// has no default to be asked for.
    fn ask_default(&mut self, app: ProcId, activation: Activation) -> bool;

    /// The window `app` most recently opened, as its channel id, for a host
    /// that means to raise it. `None` when it owns none.
    fn recent_window(&self, app: ProcId) -> Option<u64>;

    /// Spend `app`'s unspent activation, answering whether it was minted while
    /// the keyboard was at `focus`: the one raise a gesture at `app` lent it,
    /// for a host to lend what `app` hands over to instead.
    fn spend_activation(&mut self, app: ProcId, focus: Option<u64>) -> bool;
}

/// A shared-memory region a request named: the handle it was delegated under,
/// and the attested client whose delegation it must be.
///
/// Every client's grants land in the server's one table, so a handle alone
/// says nothing about whose region it is; mapping one binds it to its grantor.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ClientRegion {
    /// The client the request came from.
    pub grantor: ProcId,
    /// The region's handle in this server's grant table.
    pub handle: u64,
}

impl ClientRegion {
    /// The region `handle` a request from `grantor` named.
    #[must_use]
    pub const fn of(grantor: ProcId, handle: u64) -> Self {
        Self { grantor, handle }
    }
}

/// The file a pick concluded with — or the folder a subtree pick hands over —
/// as the session delegated it.
#[derive(Clone, Copy, Debug)]
pub struct PickedFile<'a> {
    /// The one-shot `fd_redeem` handle minted to the window's owner; never
    /// zero.
    pub handle: u64,
    /// Its own name, for the owner's `TakePickedName`.
    pub name: &'a DocumentName,
    /// Whether the descriptor it redeems may be written, or what lies beneath
    /// a folder changed.
    pub writable: bool,
}

/// One target queued for an application to open — the session's owned form
/// of [`tairix_abi::window_ipc::OpenTarget`].
///
/// The wire type borrows from the frame it is encoded into, which a queue
/// cannot hold; this is what the session queues and the engine hands back.
// The document's name is held inline, which is what makes a name the channel
// cannot carry unrepresentable; entries wait in a queue bounded per
// application, so boxing it would buy an allocation per entry and nothing else.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpenEntry {
    /// A path the user named. Confers no access.
    Path(String),
    /// A disk image or archive the user opened, to be attached and shown.
    /// Confers no access.
    Image(String),
    /// A document already opened, reachable through a one-shot delegation the
    /// session minted to the application this is queued for.
    Document {
        /// Its own file name, for a title. Empty when unknown. Bounded by its
        /// type, so no name the channel cannot carry is ever minted for.
        name: DocumentName,
        /// The `fd_redeem` handle. Never zero.
        grant: u64,
        /// Whether the descriptor it redeems is open read-write.
        writable: bool,
    },
    /// A place inside the application, resolved against its own closed set
    /// of places. Confers nothing.
    Pane(String),
    /// A file dropped on a window of the application, to be placed where it
    /// was dropped, reachable read-only through a one-shot delegation the
    /// session minted to the application.
    Placed {
        /// The window it was dropped on, and where.
        placement: Placement,
        /// Its own file name, for display.
        name: DocumentName,
        /// The `fd_redeem` handle. Never zero.
        grant: u64,
    },
}

/// One entry of the shipped wallpaper catalog, owned by the host that
/// listed the store — the session's owned form of
/// [`tairix_abi::window_ipc::WallpaperEntry`], which
/// borrows from the frame it is encoded into.
///
/// Two names rather than a path, because the *caller* builds the path from
/// them through the one shared spelling, so the catalog and the settings
/// document a chosen wallpaper produces cannot disagree about where it
/// lives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WallpaperName {
    /// The store category directory this wallpaper is filed under.
    pub category: String,
    /// The wallpaper's own file name inside that category.
    pub file: String,
}

/// One cursor set a host offers, named by the directory it occupies in the
/// shipped store — which is also the label a chooser draws.
///
/// A `String` rather than the typed `CursorSetId`: the engine relays the
/// name and judges only its length, so it needs no cursor vocabulary and
/// this crate takes on no dependency to carry one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CursorSetName(pub String);

impl OpenEntry {
    /// This entry as the wire type, borrowing its text.
    fn as_wire(&self) -> OpenTarget<'_> {
        match self {
            Self::Path(path) => OpenTarget::Path(path.as_bytes()),
            Self::Image(path) => OpenTarget::Image(path.as_bytes()),
            Self::Document {
                name,
                grant,
                writable,
            } => OpenTarget::Document {
                name: name.as_str().as_bytes(),
                grant: *grant,
                writable: *writable,
            },
            Self::Pane(pane) => OpenTarget::Pane(pane.as_bytes()),
            Self::Placed {
                placement,
                name,
                grant,
            } => OpenTarget::Placed {
                window_id: placement.window_id,
                x: placement.x,
                y: placement.y,
                name: name.as_str().as_bytes(),
                grant: *grant,
            },
        }
    }

    /// The delegation it carries, if any.
    const fn grant(&self) -> Option<u64> {
        match self {
            Self::Document { grant, .. } | Self::Placed { grant, .. } => Some(*grant),
            Self::Path(_) | Self::Image(_) | Self::Pane(_) => None,
        }
    }
}

/// One live window: its attested owner, its event route, its
/// once-mapped frame region, and whether a trusted-picker request is
/// awaiting its conclusion.
struct WindowRecord<R> {
    owner: ProcId,
    event_endpoint: u64,
    surface: DisplayMode,
    frame_count: u32,
    frame_len: usize,
    /// The mapped client frame region, or `None` while released: the session
    /// unmaps a hidden window's frames under memory pressure so the physical
    /// pages can actually go, which needs both sides to let go. The geometry
    /// stays, so the window still lays out, hit-tests, and re-attaches.
    region: Option<R>,
    /// The frame presented since the last the session showed, and the
    /// present held behind it.
    pending: Pending,
    /// A `PickFile` was accepted for this purpose and its conclusion
    /// (`FilePicked`, `FolderPicked` or `PickCancelled`) has not been
    /// delivered yet. At most one pick is pending per window; the engine sets
    /// it on acceptance and
    /// clears it when the conclusion is delivered, so the protocol's
    /// one-conclusion-per-acceptance shape is enforced in one place.
    pick_pending: Option<PickPurpose>,
    /// The name of the file the last pick chose, until the owner takes it
    /// or asks for another pick.
    picked_name: Option<DocumentName>,
    /// The files the last folder pick delegated that the owner has not
    /// taken. Never dropped untaken: a delegation cannot be withdrawn, so a
    /// further pick waits until they are taken.
    picked_files: VecDeque<tairix_abi::window_ipc::PickedFile>,
    /// A `BeginDrag` was accepted and its `DragEnded` is still owed.
    drag_pending: bool,
    /// How many items the last drag began with.
    drag_count: u32,
    /// The desktop folder the reports of the place the drag was last on
    /// named, until another drag begins.
    drag_spot: Option<DragSpot>,
    /// The drops onto the desktop the owner is still landing, the oldest
    /// first, at most [`DROP_LANDINGS_MAX`].
    landings: VecDeque<Landing>,
    /// What the last drag's drop leaves the owner to take, until it takes it
    /// or begins another.
    dropped: Option<DropHeld>,
    /// Every `RenderPreview` accepted whose conclusion is still owed, by
    /// what it renders. How many may be pending is the host's to bound —
    /// it runs the decodes — and the engine holds each acceptance to exactly
    /// one conclusion, so a picture already pending at a size is refused
    /// rather than owed twice.
    renders_pending: Vec<PreviewSize>,
    /// The id of an accepted `OpenMenu` whose outcome has not been
    /// delivered yet, or `None`. At most one open is unanswered per window;
    /// the engine records the id on acceptance and clears it when the
    /// outcome is delivered, so the protocol's one-outcome-per-acceptance
    /// shape is enforced in one place and an application can tell one
    /// gesture's answer from the next's.
    menu_open: Option<u64>,
    /// The open whose outcome was delivered last, so a restate that crossed
    /// that answer in flight is a no-op rather than a refusal.
    menu_answered: Option<u64>,
    /// The text the user committed into a menu chain's quick-entry field,
    /// with the open it belongs to, until the owning application pulls it.
    ///
    /// One slot, not a queue: a window has at most one unanswered open, so it
    /// has at most one commit to hand back, and the next open on this window
    /// clears it — a commit nobody pulled can never answer a later gesture.
    /// It is held here rather than sent in the event because an event is one
    /// fixed 40-byte frame and a name is wider than that.
    menu_text: Option<CommittedText>,
    /// The top-level window this record is a **transient** of, and of which
    /// kind, or `None` for an ordinary top-level window. A transient is
    /// closed when the window it hangs from closes, so the link lives beside
    /// the window it binds.
    transient: Option<Transient>,
    /// The desktop stacking layer this surface sits in, or `None` for an
    /// ordinary window.
    ///
    /// One field answers both "is this a layer surface?" and "which layer?",
    /// so the two can never disagree: an operation reserved for a layer
    /// surface refuses any window whose answer here is `None`.
    layer: Option<LayerDepth>,
    /// An `Authorise` was accepted and its `Authorised` is still owed.
    authorising: bool,
    /// What the last captured task printed, until the owner takes it or asks
    /// for another task.
    authorised_output: Option<Vec<u8>>,
}

/// What every newly opened window's record starts from: its owner, event
/// route and frame layout.
#[derive(Copy, Clone)]
struct Opened {
    owner: ProcId,
    event_endpoint: u64,
    surface: DisplayMode,
    frame_count: u32,
    frame_len: usize,
}

impl<R> WindowRecord<R> {
    /// The record of a window just opened over `region`, hung from
    /// `transient` or stacked at `layer` where it is one, with nothing yet
    /// pending.
    fn opened(
        opened: Opened,
        region: R,
        transient: Option<Transient>,
        layer: Option<LayerDepth>,
    ) -> Self {
        Self {
            owner: opened.owner,
            event_endpoint: opened.event_endpoint,
            surface: opened.surface,
            frame_count: opened.frame_count,
            frame_len: opened.frame_len,
            region: Some(region),
            pending: Pending::new(),
            pick_pending: None,
            picked_name: None,
            picked_files: VecDeque::new(),
            drag_pending: false,
            drag_count: 0,
            drag_spot: None,
            landings: VecDeque::new(),
            dropped: None,
            renders_pending: Vec::new(),
            menu_open: None,
            menu_answered: None,
            menu_text: None,
            transient,
            layer,
            authorising: false,
            authorised_output: None,
        }
    }
}

/// The window a transient hangs from, and what it is to that window.
#[derive(Copy, Clone)]
struct Transient {
    parent: u64,
    kind: TransientKind,
}

/// The desktop folder the reports of one place named, from the first of them
/// to the last: every report a pointer resting there makes names that folder.
struct DragSpot {
    first: u32,
    last: u32,
    folder: String,
}

impl DragSpot {
    /// Whether the report numbered `serial` was made at this place, counting
    /// across the numbers' wrap.
    const fn holds(&self, serial: u32) -> bool {
        serial.wrapping_sub(self.first) <= self.last.wrapping_sub(self.first)
    }
}

/// Hold `folder` for the report numbered `serial`: the place `spot` holds goes
/// on where the report follows its last and names its folder, and a new place
/// begins otherwise.
fn hold_spot(spot: &mut Option<DragSpot>, serial: u32, folder: &str) {
    let follows =
        |held: &&mut DragSpot| held.folder == folder && serial == held.last.wrapping_add(1);
    if let Some(held) = spot.as_mut().filter(follows) {
        held.last = serial;
        return;
    }
    *spot = Some(DragSpot {
        first: serial,
        last: serial,
        folder: String::from(folder),
    });
}

/// A drop onto the desktop a window's drag concluded, which its owner is
/// still stepping ([`WindowRequest::DropLanding`]).
struct Landing {
    serial: u32,
    /// How many items the drag began with: every step names one below.
    count: u32,
    /// The item the last `Making` named, which the next must follow.
    last: Option<u32>,
    /// Whether that item is still being made.
    making: bool,
}

impl Landing {
    /// Take `step`, or refuse it out of turn: an item past the drag's or not
    /// after the last, a `Making` while an item is made, a `Made` for any item
    /// but that one, or a name that is no file name.
    fn step(&mut self, step: &LandingStep) -> Result<(), Errno> {
        match *step {
            LandingStep::Making { item, name } => {
                let follows = self.last.is_none_or(|last| item > last);
                if self.making
                    || item >= self.count
                    || !follows
                    || tairix_path::validate_file_name(name.as_str()).is_err()
                {
                    return Err(Errno::OutOfRange);
                }
                self.last = Some(item);
                self.making = true;
            }
            LandingStep::Made { item, .. } => {
                if !self.making || self.last != Some(item) {
                    return Err(Errno::OutOfRange);
                }
                self.making = false;
            }
            LandingStep::Ended => {}
        }
        Ok(())
    }
}

/// Record the drop numbered `serial` of a drag of `count` items as landing,
/// answering the landing it ended past [`DROP_LANDINGS_MAX`]. With no memory
/// to record it, the drop lands as one never stepped does.
fn land_drop(landings: &mut VecDeque<Landing>, serial: u32, count: u32) -> Option<u32> {
    let ended = if landings.len() >= DROP_LANDINGS_MAX {
        landings.pop_front().map(|landing| landing.serial)
    } else {
        None
    };
    if landings.try_reserve(1).is_ok() {
        landings.push_back(Landing {
            serial,
            count,
            last: None,
            making: false,
        });
    }
    ended
}

/// Tell `host` every drop window `window_id` was landing has ended: nothing
/// can step one now.
fn end_landings(host: &mut dyn WindowHost, window_id: u64, landings: &VecDeque<Landing>) {
    for landing in landings {
        host.drop_landing(window_id, landing.serial, &LandingStep::Ended);
    }
}

/// What a transient is: a dialog stays until it is answered, so an alert may
/// hold it; a popup or tool window comes and goes with a gesture, so none may.
#[derive(Copy, Clone, Eq, PartialEq)]
enum TransientKind {
    Popup,
    Tool,
    Dialog,
}

/// One committed quick-entry text, with the open whose answer it belongs to.
struct CommittedText {
    open_id: u64,
    text: String,
}

/// One committed icon-bar quick-entry text, with the revision of the
/// declaration whose menu committed it.
struct CommittedRowText {
    revision: u32,
    text: String,
}

/// One alert the host has up, and where its answer goes.
struct AlertRecord {
    owner: ProcId,
    event_endpoint: u64,
    /// The owner's window the alert holds, which takes it down when it closes.
    window: Option<u64>,
    /// How many buttons it offers: the choices an answer may name.
    buttons: usize,
}

impl<R> WindowRecord<R> {
    /// Bytes of client frame region this window holds mapped in the
    /// session: every frame of it, which is what the mapping validated.
    fn mapped_bytes(&self) -> u64 {
        if self.region.is_none() {
            return 0;
        }
        (self.frame_len as u64).saturating_mul(u64::from(self.frame_count))
    }
}

/// The window-channel engine: one instance serves one desktop session.
pub struct WindowServer<M: ShmMapper> {
    mapper: M,
    /// The serving session's own kernel-attested identity, stamped into
    /// every successful create reply so an app can authenticate the
    /// sender of each later event against it (the reply channel is the
    /// squat-protected rendezvous, so the stamp is trustworthy).
    server: ProcId,
    windows: BTreeMap<u64, WindowRecord<M::Region>>,
    /// The id ranges connected clients name what they create by, keyed by
    /// prefix: the process holding each and the next serial it may name, so
    /// no id is named twice and none reaches another process's objects.
    ranges: BTreeMap<u32, (ProcId, ObjectIds)>,
    /// How many ranges each process holds, at most [`OBJECT_RANGES_MAX`].
    ranges_held: BTreeMap<ProcId, usize>,
    /// The prefix the next connect is given; zero once every prefix has been
    /// given, which refuses further connects.
    next_prefix: u32,
    /// Where each application that declared an icon-bar presence receives
    /// its bar events. The declaration's *content* is the host's — the
    /// engine keeps only the route, so an application-scoped event can be
    /// delivered without asking the host where it goes.
    app_bars: BTreeMap<ProcId, u64>,
    /// The text each application's icon-bar rows had committed and not yet
    /// taken, one per row with the revision it was committed under: at most
    /// [`APP_MENU_MAX_TOTAL_ROWS`] a client, the most rows its declaration
    /// carries.
    app_bar_texts: BTreeMap<ProcId, BTreeMap<AppMenuItemId, CommittedRowText>>,
    /// Each application's icon-bar slot authorisation, while one pends or
    /// its output waits to be taken.
    slot_authorisations: BTreeMap<ProcId, SlotAuthorisation>,
    /// Targets queued for each application to open, oldest first.
    ///
    /// Keyed on the application rather than on one of its windows, because
    /// the instance a hand-over most needs to reach is the one with nothing
    /// open: a resident single-instance viewer sitting on the icon bar has no
    /// window to queue against. Bounded per application by
    /// [`WINDOW_MAX_OPEN_TARGETS`], and the whole queue dies with the client,
    /// so a target nobody drained is reachable by nothing and is dropped.
    open_targets: BTreeMap<ProcId, VecDeque<OpenEntry>>,
    /// Applications holding an unspent activation, each with the window the
    /// keyboard was at when it was minted: one raise of the application's own
    /// windows ([`WindowRequest::ActivateWindow`]) or of what it hands over to,
    /// void once the keyboard has moved, so a gesture long past cannot be
    /// cashed in over the user's work.
    activations: BTreeMap<ProcId, Option<u64>>,
    /// The alerts up, each owed exactly one answer.
    alerts: BTreeMap<u64, AlertRecord>,
    /// Bytes of window frame one client may hold mapped here at once
    /// ([`client_frame_budget_bytes`]).
    client_frame_max: u64,
    /// The windows holding a frame [`Self::latch`] has yet to show, counted
    /// out and in around every change to one: a latch runs only while some
    /// are counted, so a frame left out would wait for another present.
    unshown: Unshown,
    /// The answers to presents held behind a frame not yet shown, and those
    /// due, which [`Self::send_answers`] sends.
    answers: Outbox,
}

impl<M: ShmMapper> WindowServer<M> {
    /// An engine with no windows, mapping through `mapper`, replying as
    /// `server` (the session's own kernel-attested `ProcId`, e.g. from
    /// `self_origin`), bounding each client to `client_frame_max` bytes of
    /// mapped window frame ([`client_frame_budget_bytes`]).
    pub const fn new(mapper: M, server: ProcId, client_frame_max: u64) -> Self {
        Self {
            mapper,
            server,
            windows: BTreeMap::new(),
            ranges: BTreeMap::new(),
            ranges_held: BTreeMap::new(),
            next_prefix: 1,
            alerts: BTreeMap::new(),
            app_bars: BTreeMap::new(),
            app_bar_texts: BTreeMap::new(),
            slot_authorisations: BTreeMap::new(),
            open_targets: BTreeMap::new(),
            activations: BTreeMap::new(),
            client_frame_max,
            unshown: Unshown::NONE,
            answers: Outbox::new(),
        }
    }

    /// Bytes of window frame `owner` currently holds mapped here, ignoring
    /// window `except` (the one a resize is replacing).
    ///
    /// A pass over the live windows rather than a running per-client tally:
    /// a create, a resize, and a close are each one mapping operation on a
    /// map of at most a few hundred entries, so the sum costs nothing
    /// measurable there, while a denormalised total is one missed update
    /// away from admitting whatever it has lost track of.
    fn client_frame_bytes(&self, owner: ProcId, except: Option<u64>) -> u64 {
        self.windows
            .iter()
            .filter(|(id, record)| record.owner == owner && Some(**id) != except)
            .map(|(_, record)| record.mapped_bytes())
            .fold(0u64, u64::saturating_add)
    }

    /// Whether `owner` may hold `bytes` more frame, with `except` excluded
    /// from what it already holds.
    fn client_frames_fit(&self, owner: ProcId, bytes: u64, except: Option<u64>) -> bool {
        self.client_frame_bytes(owner, except).saturating_add(bytes) <= self.client_frame_max
    }

    /// Number of live windows across every client.
    #[must_use]
    pub fn window_count(&self) -> usize {
        self.windows.len()
    }

    /// The attested owner of live window `window_id`, if any.
    ///
    /// The session uses this to turn a failed event delivery into the
    /// owner's [`client_exited`](Self::client_exited) teardown: the
    /// kernel reclaims a dead task's event port, so a delivery that
    /// finds no port is the kernel-backed fact that the owner is gone.
    #[must_use]
    pub fn owner_of(&self, window_id: u64) -> Option<ProcId> {
        self.windows.get(&window_id).map(|record| record.owner)
    }

    /// Handle one received request: decode `request`, attest the caller
    /// behind `ticket`, act through `host`, and write the encoded reply
    /// into `reply`.
    ///
    /// Every outcome — including a malformed request — is a well-formed
    /// reply frame; the engine never leaves a caller without an answer. A
    /// present whose window holds a frame the desktop has yet to show is
    /// [`Served::Withheld`] and answered through [`Self::send_answers`] once
    /// that frame is shown or refused, or something supersedes it, so a
    /// client drawing faster than the desktop shows frames is held to the
    /// desktop's pace.
    pub fn serve(
        &mut self,
        host: &mut dyn WindowHost,
        sink: &mut dyn EventSink,
        identity: &mut dyn CallerIdentity,
        ticket: u64,
        request: &[u8],
        reply: &mut [u8; WINDOW_REPLY_MAX],
    ) -> Served {
        let decoded = match WindowRequest::from_bytes(request) {
            Ok(decoded) => decoded,
            Err(err) => return Served::Reply(status(reply, Err(err))),
        };
        let caller = match identity.caller(ticket) {
            Ok(caller) => caller,
            Err(err) => return Served::Reply(refused(&decoded, reply, err, self.server)),
        };
        if let WindowRequest::Present {
            window_id,
            frame_index,
            damage,
        } = decoded
        {
            return match self.present((caller, ticket), window_id, frame_index, &damage) {
                Ok(true) => Served::Withheld,
                Ok(false) => Served::Reply(status(reply, Ok(()))),
                Err(err) => Served::Reply(status(reply, Err(err))),
            };
        }
        let len = self.serve_attested(host, sink, identity, (caller, ticket), &decoded, reply);
        Served::Reply(len)
    }

    /// Serve `decoded`, a request from the attested `caller` behind `ticket`
    /// answered as it is served — every request but a present — writing the
    /// encoded reply into `reply` and returning its length.
    fn serve_attested(
        &mut self,
        host: &mut dyn WindowHost,
        sink: &mut dyn EventSink,
        identity: &mut dyn CallerIdentity,
        (caller, ticket): (ProcId, u64),
        decoded: &WindowRequest,
        reply: &mut [u8; WINDOW_REPLY_MAX],
    ) -> usize {
        if let Some(named) = named_mailbox(decoded) {
            let own = identity.caller_mailbox(ticket);
            if own != Ok(named) {
                let err = own.err().unwrap_or(Errno::PermissionDenied);
                return refused(decoded, reply, err, self.server);
            }
        }
        // The one gated group on this channel, checked before dispatch
        // touches any state and re-checked on every layer operation rather
        // than only at open, so a revoked grant stops the surface at its
        // next request instead of lasting as long as the process does.
        if is_layer_op(decoded) {
            let refusal = match identity.caller_holds(ticket, CapabilityId::DESKTOP_LAYER) {
                Ok(true) => None,
                Ok(false) => Some(Errno::PermissionDenied),
                Err(err) => Some(err),
            };
            if let Some(err) = refusal {
                host.layer_refused(caller, err);
                return status(reply, Err(err));
            }
        }
        // Reserved for one application: the host decides against the
        // kernel-attested caller, a second attestation only these pay.
        match *decoded {
            WindowRequest::QueryNotifySources => {
                let answered = identity
                    .caller_app(ticket)
                    .and_then(|app| host.notify_sources(app.as_ref()));
                return notify_sources_reply(reply, answered);
            }
            WindowRequest::LockScreen => {
                let locked = identity
                    .caller_app(ticket)
                    .and_then(|app| host.lock_screen(app.as_ref()));
                return status(reply, locked);
            }
            WindowRequest::PreviewScreensaver { ref document } => {
                let shown = identity.caller_app(ticket).and_then(|app| {
                    host.screensaver_preview_requested(app.as_ref(), document.as_str())
                });
                return status(reply, shown);
            }
            WindowRequest::RenderPreview { .. } => {
                let rendered = preview_request(decoded)
                    .ok_or(Errno::NotSupported)
                    .and_then(|(to, size)| {
                        // A screensaver's picture says what the user installed,
                        // so the host is told who is asking for one.
                        let app = match size.subject {
                            PreviewSubject::Screensaver(_) => identity.caller_app(ticket)?,
                            PreviewSubject::Wallpaper(_) => None,
                        };
                        self.render_preview(host, &*sink, (caller, app.as_ref()), to, size)
                    });
                return status(reply, rendered);
            }
            WindowRequest::QueryScreensaver { index } => {
                let app = match identity.caller_app(ticket) {
                    Ok(app) => app,
                    Err(err) => return screensaver_refusal(reply, err),
                };
                let mut frame = [0u8; WINDOW_SCREENSAVER_REPLY_MAX];
                let len = host.screensaver_entry(app.as_ref(), index, &mut frame);
                // A host that answers past its own frame is refused, never
                // trusted with a slice.
                let (Some(answered), Some(out)) = (frame.get(..len), reply.get_mut(..len)) else {
                    return screensaver_refusal(reply, Errno::LengthOutOfRange);
                };
                out.copy_from_slice(answered);
                return len;
            }
            WindowRequest::TakeZoneStore => {
                let granted = identity
                    .caller_app(ticket)
                    .and_then(|app| host.zone_store(caller, app.as_ref()));
                return handle_reply(reply, granted);
            }
            WindowRequest::Authorise {
                window_id,
                ref task,
            } => {
                let window_id = window_id.map(core::num::NonZeroU64::get);
                let accepted = identity
                    .caller_app(ticket)
                    .and_then(|app| self.authorise(host, caller, app.as_ref(), window_id, task));
                return status(reply, accepted);
            }
            _ => {}
        }
        self.dispatch(host, sink, caller, decoded, reply)
    }

    /// Act on one decoded request from the attested `caller`, writing the
    /// encoded outcome into `reply` and returning its length.
    fn dispatch(
        &mut self,
        host: &mut dyn WindowHost,
        sink: &mut dyn EventSink,
        caller: ProcId,
        decoded: &WindowRequest,
        reply: &mut [u8; WINDOW_REPLY_MAX],
    ) -> usize {
        match *decoded {
            WindowRequest::Create { .. }
            | WindowRequest::CreatePopup { .. }
            | WindowRequest::CreateTool { .. }
            | WindowRequest::CreateDialog { .. }
            | WindowRequest::OpenLayer { .. }
            | WindowRequest::OpenMenu { .. }
            | WindowRequest::OpenAlert { .. } => {
                status(reply, self.dispatch_open(host, caller, decoded))
            }
            WindowRequest::Connect => {
                let connected = host
                    .desktop()
                    .and_then(|desktop| Ok((desktop, self.connect(caller)?)));
                connect_reply(reply, connected, self.server)
            }
            WindowRequest::CloseAlert { alert_id } => {
                status(reply, self.close_alert(host, sink, caller, alert_id))
            }
            WindowRequest::Close { window_id } => {
                status(reply, self.close(host, sink, caller, window_id))
            }
            WindowRequest::TakeOpenTarget => {
                let taken = self.take_open_target(caller);
                open_target_reply(reply, Ok(taken.as_ref().map(OpenEntry::as_wire)))
            }
            WindowRequest::TakeAuthorisedOutput { window_id } => {
                let window_id = window_id.map(core::num::NonZeroU64::get);
                self.take_authorised_output(caller, window_id, reply)
            }
            WindowRequest::QueryWallpapers { from } => wallpapers_reply(reply, host, from),
            WindowRequest::QueryCursorSets => cursor_sets_reply(reply, host),
            WindowRequest::TakeMenuText { window_id, open_id } => {
                let taken = self.take_menu_text(caller, window_id, open_id);
                menu_text_reply(
                    reply,
                    taken.as_ref().map(Option::as_deref).map_err(|&err| err),
                )
            }
            WindowRequest::TakeAppBarText { item, revision } => {
                let taken = self.take_app_bar_text(caller, item, revision);
                menu_text_reply(reply, Ok(taken.as_deref()))
            }
            WindowRequest::RestateMenu {
                window_id,
                open_id,
                revision,
                ref menu,
            } => status(
                reply,
                self.restate_menu(host, caller, (window_id, open_id, revision), menu),
            ),
            WindowRequest::HandOverLaunch { .. }
            | WindowRequest::RevealDocument { .. }
            | WindowRequest::PlaceDropped { .. } => {
                self.dispatch_reach(host, sink, caller, decoded, reply)
            }
            WindowRequest::PickFile { .. }
            | WindowRequest::TakePickedName { .. }
            | WindowRequest::TakePickedFile { .. }
            | WindowRequest::BeginDrag { .. }
            | WindowRequest::TakeDropTarget { .. }
            | WindowRequest::DragVerdict { .. }
            | WindowRequest::QueryDragSpot { .. }
            | WindowRequest::SaveOfferAnswer { .. }
            | WindowRequest::SaveInto { .. }
            | WindowRequest::ItemsOfferAnswer { .. }
            | WindowRequest::OfferedItems { .. }
            | WindowRequest::OfferedFile { .. }
            | WindowRequest::FileOfferAnswer { .. }
            | WindowRequest::DropLanding { .. } => {
                self.dispatch_transfer(host, caller, decoded, reply)
            }
            ref other => self.dispatch_status_op(host, caller, other, reply),
        }
    }

    /// Open what `decoded` asks `caller` to create, under the id it names.
    fn dispatch_open(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        decoded: &WindowRequest,
    ) -> Result<(), Errno> {
        match *decoded {
            WindowRequest::Create {
                window_id,
                shm_handle,
                event_endpoint,
                frame_count,
                width_px,
                height_px,
                stride_bytes,
                format,
                title,
                sizing,
            } => {
                let spec = CreateSpec {
                    window_id,
                    shm_handle,
                    event_endpoint,
                    frame_count,
                    surface: surface_of(width_px, height_px, stride_bytes, format),
                    title,
                    sizing,
                };
                self.create(host, caller, spec)
            }
            WindowRequest::CreatePopup { .. }
            | WindowRequest::CreateTool { .. }
            | WindowRequest::CreateDialog { .. } => self.create_transient(host, caller, decoded),
            WindowRequest::OpenLayer { window_id, .. } => match layer_spec(decoded) {
                Some(spec) => self.open_layer(host, caller, window_id, &spec),
                None => Err(Errno::NotSupported),
            },
            WindowRequest::OpenMenu {
                window_id,
                open_id,
                anchor,
                ref menu,
            } => self.open_menu(host, caller, (window_id, open_id), anchor, menu),
            WindowRequest::OpenAlert {
                alert_id,
                event_endpoint,
                owner_window_id,
                ref alert,
            } => self.open_alert(
                host,
                caller,
                alert_id,
                (event_endpoint, owner_window_id),
                alert,
            ),
            _ => Err(Errno::NotSupported),
        }
    }

    /// Act on a request that reaches another application's live instance
    /// through the desk the engine lends the host: a hand-over, or a reveal of
    /// a document in the file manager.
    fn dispatch_reach(
        &mut self,
        host: &mut dyn WindowHost,
        sink: &mut dyn EventSink,
        caller: ProcId,
        request: &WindowRequest,
        reply: &mut [u8; WINDOW_REPLY_MAX],
    ) -> usize {
        let mut desk = EngineDesk { server: self, sink };
        match *request {
            WindowRequest::HandOverLaunch {
                ref run_path,
                ref target,
            } => {
                let outcome =
                    host.hand_over_requested(&mut desk, caller, run_path.as_str(), target);
                hand_over_reply(reply, outcome)
            }
            WindowRequest::RevealDocument {
                window_id,
                location,
            } => status(
                reply,
                host.reveal_requested(&mut desk, caller, window_id, location),
            ),
            WindowRequest::PlaceDropped {
                window_id,
                ref name,
                grant,
            } => {
                let into = desk.server.placement_of(caller, window_id);
                let file = PlacedFile { name, grant };
                status(reply, host.place_dropped(&mut desk, caller, into, file))
            }
            _ => status(reply, Err(Errno::NotSupported)),
        }
    }

    /// Act on a request that moves a file between an application and the
    /// user — a trusted pick or a drag — each honoured only for a window the
    /// caller owns.
    fn dispatch_transfer(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        request: &WindowRequest,
        reply: &mut [u8; WINDOW_REPLY_MAX],
    ) -> usize {
        match *request {
            WindowRequest::PickFile {
                window_id,
                ref purpose,
            } => status(reply, self.pick_file(host, caller, window_id, purpose)),
            WindowRequest::TakePickedName { window_id } => {
                let taken = owned_window_mut(&mut self.windows, caller, window_id)
                    .and_then(|record| record.picked_name.take().ok_or(Errno::NotFound));
                picked_name_reply(reply, taken.as_ref().map_err(|&err| err))
            }
            WindowRequest::TakePickedFile { window_id } => {
                let taken = owned_window_mut(&mut self.windows, caller, window_id)
                    .and_then(|record| record.picked_files.pop_front().ok_or(Errno::NotFound));
                picked_file_reply(reply, taken.as_ref().map_err(|&err| err))
            }
            WindowRequest::BeginDrag {
                window_id,
                ref items,
            } => status(reply, self.begin_drag(host, caller, window_id, items)),
            WindowRequest::DragVerdict {
                window_id,
                serial,
                verdict,
            } => {
                let answered = owned_window_mut(&mut self.windows, caller, window_id)
                    .and_then(|record| {
                        if record.drag_pending {
                            Ok(())
                        } else {
                            Err(Errno::NotFound)
                        }
                    })
                    .and_then(|()| host.drag_verdict(window_id, serial, verdict));
                status(reply, answered)
            }
            WindowRequest::SaveOfferAnswer {
                window_id,
                serial,
                accepts,
            } => {
                let answered = owned_window_mut(&mut self.windows, caller, window_id)
                    .and_then(|_| host.save_offer_answered(window_id, serial, accepts));
                status(reply, answered)
            }
            WindowRequest::ItemsOfferAnswer {
                window_id,
                serial,
                verdict,
            } => {
                let answered = owned_window_mut(&mut self.windows, caller, window_id)
                    .and_then(|_| host.items_offer_answered(window_id, serial, verdict));
                status(reply, answered)
            }
            WindowRequest::OfferedItems {
                window_id,
                drag,
                from,
            } => self.offered_items(&*host, caller, (window_id, drag, from), reply),
            WindowRequest::OfferedFile { window_id, serial } => {
                let read = owned_window_mut(&mut self.windows, caller, window_id)
                    .and_then(|_| host.offered_file(window_id, serial));
                offered_file_reply(reply, read)
            }
            WindowRequest::FileOfferAnswer {
                window_id,
                serial,
                places,
            } => {
                let answered = owned_window_mut(&mut self.windows, caller, window_id)
                    .and_then(|_| host.file_offer_answered(window_id, serial, places));
                status(reply, answered)
            }
            // Not checked against the window here: the host consumes the
            // location whatever it answers, and checks the window is the
            // caller's before acting on it.
            WindowRequest::SaveInto {
                window_id,
                serial,
                location,
            } => status(reply, host.save_into(caller, window_id, serial, location)),
            WindowRequest::QueryDragSpot { window_id, serial } => {
                let read =
                    owned_window_mut(&mut self.windows, caller, window_id).and_then(|record| {
                        match &record.drag_spot {
                            Some(spot) if spot.holds(serial) => Ok(spot.folder.as_str()),
                            _ => Err(Errno::NotFound),
                        }
                    });
                drag_spot_reply(reply, read)
            }
            WindowRequest::DropLanding {
                window_id,
                serial,
                ref step,
            } => status(
                reply,
                self.step_landing(host, caller, window_id, serial, step),
            ),
            WindowRequest::TakeDropTarget { window_id } => {
                self.take_drop_target(caller, window_id, reply)
            }
            _ => status(reply, Err(Errno::NotSupported)),
        }
    }

    /// Answer `caller`'s take of the application its window `window_id`'s drag
    /// was dropped on: one-shot.
    fn take_drop_target(
        &mut self,
        caller: ProcId,
        window_id: u64,
        reply: &mut [u8; WINDOW_REPLY_MAX],
    ) -> usize {
        let taken =
            owned_window_mut(&mut self.windows, caller, window_id).and_then(|record| match record
                .dropped
                .take_if(|held| matches!(held, DropHeld::Target(_)))
            {
                Some(DropHeld::Target(target)) => Ok(target),
                _ => Err(Errno::NotFound),
            });
        let mut frame = [0u8; WINDOW_DROP_TARGET_REPLY_MAX];
        let len = encode_drop_target_reply(&mut frame, taken.as_deref().map_err(|&err| err));
        reply[..len].copy_from_slice(&frame[..len]);
        len
    }

    /// Take the placement `caller`'s window `window_id`'s drag left, with the
    /// owner of the window it goes into: one-shot, so a placed drop is handed
    /// over at most once.
    fn placement_of(
        &mut self,
        caller: ProcId,
        window_id: u64,
    ) -> Result<(ProcId, Placement), Errno> {
        let record = owned_window_mut(&mut self.windows, caller, window_id)?;
        let Some(DropHeld::Placed(placement)) = record
            .dropped
            .take_if(|held| matches!(held, DropHeld::Placed(_)))
        else {
            return Err(Errno::NotFound);
        };
        let into = self.owner_of(placement.window_id).ok_or(Errno::NotFound)?;
        Ok((into, placement))
    }

    /// Answer `caller`'s read of the items the desktop's drag numbered `drag`
    /// offers its window `window_id`, item `from` first, with as many as a
    /// page holds.
    fn offered_items(
        &mut self,
        host: &dyn WindowHost,
        caller: ProcId,
        (window_id, drag, from): (u64, u32, u32),
        reply: &mut [u8; WINDOW_REPLY_MAX],
    ) -> usize {
        let total = owned_window_mut(&mut self.windows, caller, window_id)
            .and_then(|_| host.offered_count(window_id, drag));
        let page = total.map(|total| {
            let items = (from..total)
                .map_while(move |index| host.offered_item(window_id, drag, index).ok());
            (total, items)
        });
        offered_items_reply(reply, page)
    }

    /// Accept a drag `caller`'s window `window_id` began on the file `name`:
    /// at most one drag pends per window, and the host must take the gesture
    /// over before anything is recorded.
    fn begin_drag(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        window_id: u64,
        items: &DragItems,
    ) -> Result<(), Errno> {
        let record = owned_window_mut(&mut self.windows, caller, window_id)?;
        if record.drag_pending {
            return Err(Errno::AlreadyExists);
        }
        host.drag_requested(window_id, items)?;
        record.drag_pending = true;
        record.drag_count = items.count();
        record.drag_spot = None;
        record.dropped = None;
        Ok(())
    }

    /// Take `step` of the drop numbered `serial` that `caller`'s window
    /// `window_id` is landing on the desktop, and tell the host: the one way
    /// a landing moves, so a step out of turn reaches nothing.
    fn step_landing(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        window_id: u64,
        serial: u32,
        step: &LandingStep,
    ) -> Result<(), Errno> {
        let landings = &mut owned_window_mut(&mut self.windows, caller, window_id)?.landings;
        landings
            .iter_mut()
            .find(|landing| landing.serial == serial)
            .ok_or(Errno::NotFound)?
            .step(step)?;
        if matches!(step, LandingStep::Ended) {
            landings.retain(|landing| landing.serial != serial);
        }
        host.drop_landing(window_id, serial, step);
        Ok(())
    }

    /// Act on a request a window makes of the seat — the tip shown over its
    /// content, the pointer's shape there, the keyboard, the clipboard — each
    /// honoured only for a window the caller owns.
    fn dispatch_seat(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        request: &WindowRequest,
        reply: &mut [u8; WINDOW_REPLY_MAX],
    ) -> usize {
        if let WindowRequest::SetTooltip {
            window_id,
            region,
            ref text,
        } = *request
        {
            return status(
                reply,
                self.set_tooltip(host, caller, window_id, region, text.as_str()),
            );
        }
        if let WindowRequest::ActivateWindow { window_id } = *request {
            return status(reply, self.activate(host, caller, window_id));
        }
        let owns = |window_id| self.owns(caller, window_id);
        match *request {
            WindowRequest::GetClipboard {
                window_id,
                shm_handle,
            } => {
                let held = if owns(window_id) {
                    host.clipboard_get(window_id, ClientRegion::of(caller, shm_handle))
                } else {
                    Err(Errno::NotFound)
                };
                let frame = encode_clipboard_reply(held);
                reply[..frame.len()].copy_from_slice(&frame);
                frame.len()
            }
            WindowRequest::SetCursor { window_id, shape } if owns(window_id) => {
                status(reply, host.cursor_set(window_id, shape))
            }
            WindowRequest::SetKeyboardMode { window_id, mode } if owns(window_id) => {
                status(reply, host.keyboard_mode_set(window_id, mode))
            }
            WindowRequest::LockPointer { window_id, locked } if owns(window_id) => {
                status(reply, host.pointer_lock_set(window_id, locked))
            }
            WindowRequest::SetClipboard {
                window_id,
                shm_handle,
                len,
                kind,
            } if owns(window_id) => status(
                reply,
                host.clipboard_set(window_id, ClientRegion::of(caller, shm_handle), len, kind),
            ),
            WindowRequest::SetCursor { .. }
            | WindowRequest::SetKeyboardMode { .. }
            | WindowRequest::LockPointer { .. }
            | WindowRequest::SetClipboard { .. } => status(reply, Err(Errno::NotFound)),
            _ => status(reply, Err(Errno::NotSupported)),
        }
    }

    /// Act on a request that sets how one of the caller's windows is shown:
    /// its size, title, sizing, size state, icon-bar declaration or backdrop.
    fn dispatch_setting(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        request: &WindowRequest,
    ) -> Result<(), Errno> {
        match *request {
            WindowRequest::Resize { .. } => {
                let spec = resize_request(request).ok_or(Errno::NotSupported)?;
                self.resize(host, caller, spec)
            }
            WindowRequest::SetTitle { window_id, title } => {
                self.set_title(host, caller, window_id, title.as_str())
            }
            WindowRequest::SetSizing { window_id, sizing } => {
                self.set_sizing(host, caller, window_id, sizing)
            }
            WindowRequest::SetSizeState { window_id, state } => {
                self.set_size_state(host, caller, window_id, state)
            }
            WindowRequest::SetAppBar(ref bar) => self.set_app_bar(host, caller, bar),
            WindowRequest::SetBackdropBlur {
                window_id,
                radius_px,
            } => self.set_backdrop_blur(host, caller, window_id, radius_px),
            _ => Err(Errno::NotSupported),
        }
    }

    /// Act on one decoded request that answers with a plain status frame —
    /// everything but the two create requests, which mint a window id and so
    /// answer with the create-reply frame instead.
    fn dispatch_status_op(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        decoded: &WindowRequest,
        reply: &mut [u8; WINDOW_REPLY_MAX],
    ) -> usize {
        match *decoded {
            WindowRequest::PlaceLayer {
                window_id,
                x,
                y,
                depth,
            } => status(
                reply,
                self.place_layer(host, caller, window_id, x, y, depth),
            ),
            WindowRequest::TakeTerrain { window_id } => {
                self.take_terrain(host, caller, window_id, reply)
            }
            WindowRequest::SetTooltip { .. }
            | WindowRequest::SetCursor { .. }
            | WindowRequest::SetKeyboardMode { .. }
            | WindowRequest::LockPointer { .. }
            | WindowRequest::ActivateWindow { .. }
            | WindowRequest::SetClipboard { .. }
            | WindowRequest::GetClipboard { .. } => {
                self.dispatch_seat(host, caller, decoded, reply)
            }
            WindowRequest::Resize { .. }
            | WindowRequest::SetTitle { .. }
            | WindowRequest::SetSizing { .. }
            | WindowRequest::SetSizeState { .. }
            | WindowRequest::SetAppBar(_)
            | WindowRequest::SetBackdropBlur { .. } => {
                status(reply, self.dispatch_setting(host, caller, decoded))
            }
            // Each is answered before dispatch reaches here; refusing it keeps
            // this total without a second copy of its path.
            WindowRequest::Connect => connect_reply(reply, Err(Errno::NotSupported), self.server),
            WindowRequest::TakeZoneStore => handle_reply(reply, Err(Errno::NotSupported)),
            // ...and a target pull, which answers with its own frame.
            WindowRequest::TakeOpenTarget => open_target_reply(reply, Err(Errno::NotSupported)),
            // ...and a catalog page, likewise.
            WindowRequest::QueryWallpapers { .. } => wallpapers_refusal(reply, Errno::NotSupported),
            WindowRequest::QueryScreensaver { .. } => {
                screensaver_refusal(reply, Errno::NotSupported)
            }
            WindowRequest::QueryCursorSets => cursor_sets_refusal(reply, Errno::NotSupported),
            // ...and the requests answered before this is reached: a present
            // and those decided against the attested application, which
            // `serve` takes, and those `dispatch` routes to their own groups.
            WindowRequest::QueryNotifySources => {
                notify_sources_reply(reply, Err(Errno::NotSupported))
            }
            WindowRequest::Create { .. }
            | WindowRequest::CreatePopup { .. }
            | WindowRequest::CreateTool { .. }
            | WindowRequest::CreateDialog { .. }
            | WindowRequest::OpenLayer { .. }
            | WindowRequest::OpenMenu { .. }
            | WindowRequest::RestateMenu { .. }
            | WindowRequest::OpenAlert { .. }
            | WindowRequest::Present { .. }
            | WindowRequest::LockScreen
            | WindowRequest::Authorise { .. }
            | WindowRequest::TakeAuthorisedOutput { .. }
            | WindowRequest::Close { .. }
            | WindowRequest::CloseAlert { .. }
            | WindowRequest::PreviewScreensaver { .. }
            | WindowRequest::RenderPreview { .. }
            | WindowRequest::PickFile { .. }
            | WindowRequest::BeginDrag { .. }
            | WindowRequest::TakeDropTarget { .. }
            | WindowRequest::DragVerdict { .. }
            | WindowRequest::QueryDragSpot { .. }
            | WindowRequest::SaveOfferAnswer { .. }
            | WindowRequest::SaveInto { .. }
            | WindowRequest::ItemsOfferAnswer { .. }
            | WindowRequest::OfferedItems { .. }
            | WindowRequest::OfferedFile { .. }
            | WindowRequest::FileOfferAnswer { .. }
            | WindowRequest::PlaceDropped { .. }
            | WindowRequest::DropLanding { .. }
            | WindowRequest::RevealDocument { .. } => status(reply, Err(Errno::NotSupported)),
            // ...and a committed-text pull, likewise.
            WindowRequest::TakeMenuText { .. } | WindowRequest::TakeAppBarText { .. } => {
                menu_text_reply(reply, Err(Errno::NotSupported))
            }
            WindowRequest::TakePickedName { .. } => {
                picked_name_reply(reply, Err(Errno::NotSupported))
            }
            WindowRequest::TakePickedFile { .. } => {
                picked_file_reply(reply, Err(Errno::NotSupported))
            }

            // ...and a hand-over, likewise.
            WindowRequest::HandOverLaunch { .. } => {
                hand_over_reply(reply, Err(Errno::NotSupported))
            }
        }
    }

    /// Every live window id, across every client, in ascending order.
    ///
    /// The session uses this to reach each window when it must tell every
    /// application something about the desktop they share — a light/dark
    /// switch, a scale or mode change — routing each one through its own
    /// delivery path so a client that has died is torn down exactly as it
    /// would be for any other event.
    #[must_use]
    pub fn window_ids(&self) -> Vec<u64> {
        self.windows.keys().copied().collect()
    }

    /// Open a window for `caller`, named `spec.window_id`: admit the id,
    /// bound the client, map the granted region once, validate it holds
    /// every frame, and tell the host.
    fn create(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        spec: CreateSpec,
    ) -> Result<(), Errno> {
        // A window belongs to a user process instance; the kernel
        // sentinel is not a client and would alias every in-kernel
        // caller onto one "owner".
        if caller.is_kernel() {
            return Err(Errno::PermissionDenied);
        }
        self.admit(caller, spec.window_id)?;
        let window_id = spec.window_id;
        let frame_len = frame_bytes(&spec.surface)?;
        let total = frame_len
            .checked_mul(spec.frame_count as usize)
            .ok_or(Errno::LengthOutOfRange)?;
        if !self.client_frames_fit(caller, total as u64, None) {
            return Err(Errno::NoSpace);
        }
        let region = self.mapper.map(caller, spec.shm_handle, total)?;
        // Room for the answer a present of it may hold, so that answer falls
        // due without allocating.
        self.answers.reserve(self.windows.len().saturating_add(1))?;
        // Tell the host before committing: a refused open leaves no
        // record and drops the mapping (the mapper's cue to unmap).
        host.window_opened(
            caller,
            window_id,
            &spec.surface,
            spec.title.as_str(),
            spec.sizing,
        )?;
        let opened = Opened {
            owner: caller,
            event_endpoint: spec.event_endpoint,
            surface: spec.surface,
            frame_count: spec.frame_count,
            frame_len,
        };
        self.windows
            .insert(window_id, WindowRecord::opened(opened, region, None, None));
        Ok(())
    }

    /// A fresh id range for the client of `caller` that is connecting.
    fn connect(&mut self, caller: ProcId) -> Result<ObjectIds, Errno> {
        if caller.is_kernel() {
            return Err(Errno::PermissionDenied);
        }
        let held = self.ranges_held.get(&caller).copied().unwrap_or(0);
        if held >= OBJECT_RANGES_MAX {
            return Err(Errno::LimitExceeded);
        }
        let prefix = core::num::NonZeroU32::new(self.next_prefix).ok_or(Errno::NoSpace)?;
        let ids = ObjectIds::new(prefix, 1).ok_or(Errno::NoSpace)?;
        self.ranges.insert(prefix.get(), (caller, ids));
        self.ranges_held.insert(caller, held + 1);
        self.next_prefix = self.next_prefix.checked_add(1).unwrap_or(0);
        Ok(ids)
    }

    /// Admit `id` as the name `caller` gives what it is creating.
    ///
    /// # Errors
    ///
    /// [`Errno::NotConnected`] for a process holding no range, and
    /// [`Errno::OutOfRange`] for an id outside its ranges or one it has
    /// named before ([`ObjectIds::admit`]).
    fn admit(&mut self, caller: ProcId, id: u64) -> Result<(), Errno> {
        let prefix = id.to_be_bytes();
        let prefix = u32::from_be_bytes([prefix[0], prefix[1], prefix[2], prefix[3]]);
        match self.ranges.get_mut(&prefix) {
            Some((owner, ids)) if *owner == caller => ids.admit(id),
            _ if self.ranges_held.contains_key(&caller) => Err(Errno::OutOfRange),
            _ => Err(Errno::NotConnected),
        }
    }

    /// Open the popup, tool window or dialog `decoded` asks for, for
    /// `caller`, a dialog given the keyboard only where the caller has earned
    /// the front.
    fn create_transient(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        decoded: &WindowRequest,
    ) -> Result<(), Errno> {
        match *decoded {
            WindowRequest::CreatePopup {
                window_id,
                parent_window_id,
                shm_handle,
                event_endpoint,
                frame_count,
                width_px,
                height_px,
                stride_bytes,
                format,
                offset_x,
                offset_y,
            } => {
                let spec = TransientSpec {
                    parent_window_id,
                    shm_handle,
                    event_endpoint,
                    frame_count,
                    surface: surface_of(width_px, height_px, stride_bytes, format),
                };
                let kind = TransientKind::Popup;
                self.open_transient(host, (caller, window_id), &spec, kind, |host, window_id| {
                    host.popup_opened(
                        window_id,
                        parent_window_id,
                        offset_x,
                        offset_y,
                        &spec.surface,
                    )
                })
            }
            WindowRequest::CreateTool {
                window_id,
                parent_window_id,
                shm_handle,
                event_endpoint,
                frame_count,
                width_px,
                height_px,
                stride_bytes,
                format,
                offset_x,
                offset_y,
                carry,
                title,
                sizing,
            } => {
                let spec = ToolSpec {
                    transient: TransientSpec {
                        parent_window_id,
                        shm_handle,
                        event_endpoint,
                        frame_count,
                        surface: surface_of(width_px, height_px, stride_bytes, format),
                    },
                    offset_x,
                    offset_y,
                    carry,
                    title,
                    sizing,
                };
                let kind = TransientKind::Tool;
                let named = (caller, window_id);
                self.open_transient(host, named, &spec.transient, kind, |host, window_id| {
                    host.tool_opened(window_id, &spec)
                })
            }
            WindowRequest::CreateDialog {
                window_id,
                owner_window_id,
                shm_handle,
                event_endpoint,
                frame_count,
                width_px,
                height_px,
                stride_bytes,
                format,
                modality,
                title,
            } => {
                let spec = DialogSpec {
                    transient: TransientSpec {
                        parent_window_id: owner_window_id,
                        shm_handle,
                        event_endpoint,
                        frame_count,
                        surface: surface_of(width_px, height_px, stride_bytes, format),
                    },
                    modality,
                    title,
                };
                self.open_dialog(host, (caller, window_id), &spec)
            }
            _ => Err(Errno::NotSupported),
        }
    }

    /// Open the dialog `spec` describes, given the keyboard only where
    /// `caller` has earned the front.
    fn open_dialog(
        &mut self,
        host: &mut dyn WindowHost,
        (caller, window_id): (ProcId, u64),
        spec: &DialogSpec,
    ) -> Result<(), Errno> {
        // Earned as an alert's is, and spent the same way: one activation
        // brings one surface over the user's work.
        let focus = self.holds_the_front(host, caller);
        let kind = TransientKind::Dialog;
        let named = (caller, window_id);
        self.open_transient(host, named, &spec.transient, kind, |host, window_id| {
            host.dialog_opened(window_id, focus, spec)
        })?;
        if focus {
            self.activations.remove(&caller);
        }
        Ok(())
    }

    /// Open a transient of `kind` for `caller`, hung from the caller's own
    /// top-level window `spec.parent_window_id`, telling the host through
    /// `opened` before anything is committed.
    ///
    /// A transient is validated exactly like a top-level [`Self::create`] —
    /// no kernel caller, the geometry holds every frame — and its parent must
    /// be a window the caller owns that hangs from nothing itself and is not
    /// a layer surface: a foreign or unknown parent answers `NotFound`,
    /// leaking nothing, and a transient's transient is refused `NotSupported`,
    /// because the stacking that keeps a family together is one level deep. Its frames
    /// are charged against the **same** per-client budget as a top-level
    /// window's ([`client_frame_budget_bytes`]). A refused transient leaves
    /// no record and drops the mapping (fail closed).
    fn open_transient(
        &mut self,
        host: &mut dyn WindowHost,
        (caller, window_id): (ProcId, u64),
        spec: &TransientSpec,
        kind: TransientKind,
        opened: impl FnOnce(&mut dyn WindowHost, u64) -> Result<(), Errno>,
    ) -> Result<(), Errno> {
        if caller.is_kernel() {
            return Err(Errno::PermissionDenied);
        }
        self.admit(caller, window_id)?;
        let parent = owned_window(&self.windows, caller, spec.parent_window_id)?;
        if parent.transient.is_some() || parent.layer.is_some() {
            return Err(Errno::NotSupported);
        }
        let frame_len = frame_bytes(&spec.surface)?;
        let total = frame_len
            .checked_mul(spec.frame_count as usize)
            .ok_or(Errno::LengthOutOfRange)?;
        if !self.client_frames_fit(caller, total as u64, None) {
            return Err(Errno::NoSpace);
        }
        let region = self.mapper.map(caller, spec.shm_handle, total)?;
        self.answers.reserve(self.windows.len().saturating_add(1))?;
        opened(host, window_id)?;
        let transient = Transient {
            parent: spec.parent_window_id,
            kind,
        };
        let opened = Opened {
            owner: caller,
            event_endpoint: spec.event_endpoint,
            surface: spec.surface,
            frame_count: spec.frame_count,
            frame_len,
        };
        self.windows.insert(
            window_id,
            WindowRecord::opened(opened, region, Some(transient), None),
        );
        Ok(())
    }

    /// Open a desktop layer surface for `caller`.
    ///
    /// The caller's `CAP_DESKTOP_LAYER` was checked before this was
    /// reached. What is left is containment: a layer surface is counted
    /// per client and per seat, so neither one holder nor a crowd of them
    /// can occupy more of the desktop than the bounds allow, and its frames
    /// are charged against the same per-client budget every window's are.
    /// The host is told before committing, so a refused surface leaves no
    /// record and drops the mapping.
    fn open_layer(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        window_id: u64,
        spec: &LayerSpec,
    ) -> Result<(), Errno> {
        if caller.is_kernel() {
            return Err(Errno::PermissionDenied);
        }
        self.admit(caller, window_id)?;
        if self.layer_count(Some(caller)) >= DESKTOP_LAYER_MAX_PER_CLIENT
            || self.layer_count(None) >= DESKTOP_LAYER_MAX_PER_SEAT
        {
            return Err(Errno::LimitExceeded);
        }
        let frame_len = frame_bytes(&spec.surface)?;
        let total = frame_len
            .checked_mul(spec.frame_count as usize)
            .ok_or(Errno::LengthOutOfRange)?;
        if !self.client_frames_fit(caller, total as u64, None) {
            return Err(Errno::NoSpace);
        }
        let region = self.mapper.map(caller, spec.shm_handle, total)?;
        self.answers.reserve(self.windows.len().saturating_add(1))?;
        host.layer_opened(caller, window_id, &spec.surface, spec.x, spec.y, spec.depth)?;
        let opened = Opened {
            owner: caller,
            event_endpoint: spec.event_endpoint,
            surface: spec.surface,
            frame_count: spec.frame_count,
            frame_len,
        };
        self.windows.insert(
            window_id,
            WindowRecord::opened(opened, region, None, Some(spec.depth)),
        );
        Ok(())
    }

    /// How many live layer surfaces there are, for one `owner` or across
    /// every client when `owner` is `None`.
    fn layer_count(&self, owner: Option<ProcId>) -> usize {
        self.windows
            .values()
            .filter(|record| {
                record.layer.is_some() && owner.is_none_or(|caller| record.owner == caller)
            })
            .count()
    }

    /// Move `caller`'s own live layer surface to a new screen point and
    /// stacking layer.
    ///
    /// Refuses any window the caller does not own, and any window of the
    /// caller's that is not a layer surface — an ordinary window is placed
    /// by the window manager, and letting this reposition one would hand
    /// every `CAP_DESKTOP_LAYER` holder the authority to move its own
    /// windows around the screen, which is not what the capability is for.
    fn place_layer(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        window_id: u64,
        x: i32,
        y: i32,
        depth: LayerDepth,
    ) -> Result<(), Errno> {
        let record = owned_window(&self.windows, caller, window_id)?;
        if record.layer.is_none() {
            return Err(Errno::NotSupported);
        }
        host.layer_placed(window_id, x, y, depth)?;
        // Recorded only once the host accepted, so the engine's depth is
        // always the one the compositor actually stacked.
        if let Some(record) = self.windows.get_mut(&window_id) {
            record.layer = Some(depth);
        }
        Ok(())
    }

    /// Answer `caller`'s terrain pull for its own live layer surface.
    fn take_terrain(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        window_id: u64,
        reply: &mut [u8; WINDOW_REPLY_MAX],
    ) -> usize {
        let plates = match owned_window(&self.windows, caller, window_id) {
            Ok(record) if record.layer.is_none() => Err(Errno::NotSupported),
            Ok(_) => Ok(()),
            Err(err) => Err(err),
        };
        if let Err(err) = plates {
            return status(reply, Err(err));
        }
        let mut out = [TerrainPlate {
            x: 0,
            y: 0,
            width_px: 1,
            height_px: 1,
        }; DESKTOP_LAYER_MAX_PLATES as usize];
        let written = match host.layer_terrain(window_id, &mut out) {
            Ok(written) => written.min(out.len()),
            Err(err) => return status(reply, Err(err)),
        };
        match encode_terrain_reply(&out[..written], reply) {
            Ok(len) => len,
            Err(err) => status(reply, Err(err)),
        }
    }

    /// Hang an attached window from a panel row of `caller`'s live chain,
    /// returning the minted window id.
    ///
    /// The chain is the scope: the named window must be the caller's own and
    /// must hold `open_id` unanswered, so a surface for a chain that has
    /// already closed is unrepresentable rather than merely refused. One
    /// panel hangs per chain — a chain's deepest child is a plate or a
    /// panel, never both — and a second while one is live is refused. The
    /// host decides whether the pointer is still on the row and must accept
    /// before anything is recorded, so a late or refused panel leaves no
    /// window, no mapping, and no id spent.
    /// Re-map `caller`'s window `window_id` onto a fresh frame region of
    /// the new geometry, keeping its id, owner, event route, and pending
    /// pick state.
    ///
    /// The new region is mapped and validated to hold every frame before
    /// anything is committed; the host is told before the swap, so a
    /// refused resize leaves the old mapping intact (fail closed). On
    /// success the old region is dropped (the mapper's cue to unmap) as
    /// the record adopts the new one.
    fn resize(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        spec: ResizeSpec,
    ) -> Result<(), Errno> {
        // Ownership first: a window the caller does not own answers
        // exactly like one that never existed, leaking nothing.
        owned_window(&self.windows, caller, spec.window_id)?;
        let frame_len = frame_bytes(&spec.surface)?;
        let total = frame_len
            .checked_mul(spec.frame_count as usize)
            .ok_or(Errno::LengthOutOfRange)?;
        // The new geometry replaces this window's own frames, so it is the
        // one excluded from what the client already holds. A resize is how a
        // client grows its mapped bytes without opening a window, which is
        // exactly what a per-client *count* could never bound.
        if !self.client_frames_fit(caller, total as u64, Some(spec.window_id)) {
            return Err(Errno::NoSpace);
        }
        let region = self.mapper.map(caller, spec.shm_handle, total)?;
        // Tell the host before committing: a refused resize drops the
        // freshly mapped region and leaves the record's old geometry.
        host.window_resized(spec.window_id, &spec.surface)?;
        // `owned_window` proved the record exists and the borrow above is
        // released, so this re-lookup cannot miss.
        if let Some(record) = self.windows.get_mut(&spec.window_id) {
            record.surface = spec.surface;
            record.frame_count = spec.frame_count;
            record.frame_len = frame_len;
            record.region = Some(region);
            // What was pending named a frame of the region just dropped, and
            // the present held behind it was recorded, so it is answered.
            self.unshown.count_out(&record.pending);
            record.pending.clear();
            self.answers.release(&mut record.pending, record.owner);
        }
        Ok(())
    }

    /// Present one frame of `caller`'s window `window_id`, the call answered
    /// as `ticket`, answering whether that answer is withheld.
    ///
    /// Recorded, not shown: [`Self::latch`] shows it with the desktop's next
    /// frame. A window holding no unshown frame is answered at once, so the
    /// client may draw its other frame while this one waits — never the one
    /// named here, which the session reads until a later present replaces it.
    /// One replacing an unshown frame is held until the frame it names is
    /// shown or refused, or something supersedes it: answered, its client
    /// draws again, so one drawing faster than the desktop shows frames draws
    /// at most two a desktop frame rather than frames nobody sees.
    fn present(
        &mut self,
        (caller, ticket): (ProcId, u64),
        window_id: u64,
        frame_index: u32,
        damage: &DamageList,
    ) -> Result<bool, Errno> {
        let record = owned_window_mut(&mut self.windows, caller, window_id)?;
        if frame_index >= record.frame_count {
            return Err(Errno::OutOfRange);
        }
        for rect in damage.rects() {
            rect.validate_in(&record.surface)
                .map_err(|_| Errno::LengthOutOfRange)?;
        }
        // A window whose frames the session released holds no pixels to
        // present. Refusing typed is what lets a client re-attach on its next
        // paint instead of writing into a mapping neither side has.
        if record.region.is_none() {
            return Err(Errno::NotAttached);
        }
        let replaces = record.pending.frame.is_some();
        self.unshown.count_out(&record.pending);
        record.pending.record(frame_index, damage, &record.surface);
        self.unshown.count_in(&record.pending);
        Ok(replaces && self.answers.hold(&mut record.pending, caller, ticket))
    }

    /// Show every window's newest presented frame through `host`, once a
    /// desktop frame: a client presenting faster than the desktop draws costs
    /// one conversion a frame, never one a present. A refused frame is tried
    /// again with each, until it shows or a later present replaces it. Either
    /// way the present held behind the frame falls due: it was recorded.
    pub fn latch(&mut self, host: &mut dyn FrameHost) {
        if self.unshown.is_empty() {
            return;
        }
        let mut unshown = Unshown::NONE;
        for (&window_id, record) in &mut self.windows {
            if let Some(frame_index) = record.pending.frame {
                let shown = Self::frame_of(record, frame_index).and_then(|frame| {
                    let damage = damage_of(&record.pending.damage, &record.surface)?;
                    host.window_presented(window_id, &record.surface, frame, &damage)
                });
                match shown {
                    Ok(()) => record.pending.shown(),
                    Err(errno) => {
                        if record.pending.refuse() {
                            host.frame_refused(window_id, errno);
                        }
                    }
                }
                self.answers.release(&mut record.pending, record.owner);
            }
            unshown.count_in(&record.pending);
        }
        self.unshown = unshown;
    }

    /// Whether a window presented a frame that asks the desktop for a frame:
    /// one [`Self::latch`] has not tried. A refused frame waits for a frame
    /// the desktop composes for something else.
    #[must_use]
    pub const fn has_pending_frames(&self) -> bool {
        self.unshown.due > 0
    }

    /// Whether a present's answer is held behind a frame the desktop has yet
    /// to show.
    #[must_use]
    pub const fn holds_answers(&self) -> bool {
        self.answers.held > 0
    }

    /// Answer every held present without showing its frame, which stays
    /// pending for the desktop's next: the session's turn while no frame can
    /// reach the screen, taken once an output period, so a client is held to
    /// the same pace with the screen dark as lit and never stalls behind it.
    pub fn release_answers(&mut self) {
        if self.answers.held == 0 {
            return;
        }
        for record in self.windows.values_mut() {
            self.answers.release(&mut record.pending, record.owner);
        }
    }

    /// Send every answer due through `reply`, which carries a ticket's reply
    /// frame to its caller, oldest first. Each is a withheld present's, so
    /// `Ok`: the present was recorded and its client may draw its other
    /// frame. A reply lands in the room its call was posted with, so the
    /// kernel refuses one only for a caller no longer waiting, and every
    /// answer goes once sent.
    pub fn send_answers(&mut self, reply: impl FnMut(u64, &[u8])) {
        self.answers.send(reply);
    }

    /// Frame `frame_index` of `record`'s region.
    ///
    /// # Errors
    ///
    /// [`Errno::NotAttached`] for a released region, and
    /// [`Errno::DeviceFault`] for one that no longer holds the frame: the
    /// region was validated to hold every frame when it was mapped, so one
    /// that shrank is a fault, not a clamp.
    fn frame_of(record: &WindowRecord<M::Region>, frame_index: u32) -> Result<&[u8], Errno> {
        let bytes = record.region.as_ref().ok_or(Errno::NotAttached)?.bytes();
        let base = (frame_index as usize)
            .checked_mul(record.frame_len)
            .ok_or(Errno::DeviceFault)?;
        bytes
            .get(base..base.saturating_add(record.frame_len))
            .ok_or(Errno::DeviceFault)
    }

    /// Accept a trusted-picker request for `caller`'s window `window_id`:
    /// at most one pick pends per window and
    /// [`PICK_MAX_OPEN`](tairix_abi::window_ipc::PICK_MAX_OPEN) per
    /// application, one of them a folder's, each refusal told to the host to
    /// audit; and the host must be able to run its picker before anything is
    /// recorded (fail closed — a refused request leaves no pending state).
    /// The picker earns the keyboard as an alert does, spending the
    /// activation that earned it.
    fn pick_file(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        window_id: u64,
        purpose: &PickPurpose,
    ) -> Result<(), Errno> {
        // Owned-window check first: a window the caller does not own
        // answers exactly like one that never existed.
        owned_window(&self.windows, caller, window_id)?;
        if let Some((refused, reason)) = self.pick_refusal(caller, window_id, purpose) {
            host.pick_refused(caller, window_id, reason);
            return Err(refused);
        }
        let focus = self.holds_the_front(host, caller);
        // Tell the host before committing: a refused picker (the seat held,
        // no filesystem authority) leaves no pending pick behind.
        host.pick_requested(window_id, purpose, focus)?;
        if focus {
            self.activations.remove(&caller);
        }
        let record = owned_window_mut(&mut self.windows, caller, window_id)?;
        record.pick_pending = Some(*purpose);
        record.picked_name = None;
        Ok(())
    }

    /// Why `caller`'s pick for `purpose` on its window `window_id` is past a
    /// bound, if it is, and the refusal it is answered with. A folder pick
    /// whose files are delegated and not yet taken is still open, since a
    /// delegation cannot be withdrawn.
    fn pick_refusal(
        &self,
        caller: ProcId,
        window_id: u64,
        purpose: &PickPurpose,
    ) -> Option<(Errno, &'static str)> {
        let folder = |record: &WindowRecord<M::Region>| {
            record.pick_pending == Some(PickPurpose::Folder) || !record.picked_files.is_empty()
        };
        let open =
            |record: &WindowRecord<M::Region>| record.pick_pending.is_some() || folder(record);
        if self.windows.get(&window_id).is_some_and(open) {
            return Some((Errno::AlreadyExists, PICK_ON_WINDOW));
        }
        let mine = || {
            self.windows
                .values()
                .filter(move |record| record.owner == caller)
        };
        if mine().filter(|&record| open(record)).count() >= tairix_abi::window_ipc::PICK_MAX_OPEN {
            return Some((Errno::LimitExceeded, PICKS_PER_APPLICATION));
        }
        if matches!(purpose, PickPurpose::Folder) && mine().any(folder) {
            return Some((Errno::LimitExceeded, FOLDER_PICK_OPEN));
        }
        None
    }

    /// Answer `caller`'s take of what window `window_id`'s last task
    /// printed: once, and only to the window's owner.
    fn take_authorised_output(
        &mut self,
        caller: ProcId,
        window_id: Option<u64>,
        reply: &mut [u8; WINDOW_REPLY_MAX],
    ) -> usize {
        let taken = match window_id {
            Some(window_id) => owned_window_mut(&mut self.windows, caller, window_id)
                .and_then(|record| record.authorised_output.take().ok_or(Errno::NotFound)),
            None => self.take_slot_output(caller),
        };
        authorised_output_reply(reply, taken.as_deref().map_err(|&err| err))
    }

    /// What `caller`'s icon-bar slot's last task printed, letting the slot's
    /// record go once nothing pends there either.
    fn take_slot_output(&mut self, caller: ProcId) -> Result<Vec<u8>, Errno> {
        let slot = self
            .slot_authorisations
            .get_mut(&caller)
            .ok_or(Errno::NotFound)?;
        let output = slot.output.take().ok_or(Errno::NotFound)?;
        if !slot.pending {
            self.slot_authorisations.remove(&caller);
        }
        Ok(output)
    }

    /// Accept an authorisation of `task` for `caller`'s window `window_id`, or
    /// its icon-bar slot when `None`, asked by the attested application `app`:
    /// at most one pends per window and one per slot, and the host must bring
    /// its prompt up before anything is recorded (fail closed — a refused
    /// request leaves no pending state).
    fn authorise(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        app: Option<&AppIdentity>,
        window_id: Option<u64>,
        task: &SystemTask,
    ) -> Result<(), Errno> {
        let Some(window_id) = window_id else {
            if !self.app_bars.contains_key(&caller) {
                return Err(Errno::NotFound);
            }
            if self
                .slot_authorisations
                .get(&caller)
                .is_some_and(|slot| slot.pending)
            {
                return Err(Errno::AlreadyExists);
            }
            host.authorise_requested(app, AuthoriseFor::Slot(caller), task)?;
            self.slot_authorisations.insert(
                caller,
                SlotAuthorisation {
                    pending: true,
                    output: None,
                },
            );
            return Ok(());
        };
        let record = owned_window_mut(&mut self.windows, caller, window_id)?;
        if record.authorising {
            return Err(Errno::AlreadyExists);
        }
        host.authorise_requested(app, AuthoriseFor::Window(window_id), task)?;
        record.authorising = true;
        record.authorised_output = None;
        Ok(())
    }

    /// Accept a preview-render request for `caller`'s window `window_id`,
    /// into the region granted as `shm_handle`: a picture pends at most once
    /// per size, none is accepted while a conclusion already waits in `sink`
    /// for the window, and the host must accept it before anything is
    /// recorded (fail closed — a refused request leaves no pending state, so
    /// the caller may ask again).
    fn render_preview(
        &mut self,
        host: &mut dyn WindowHost,
        sink: &dyn EventSink,
        (caller, app): (ProcId, Option<&AppIdentity>),
        (window_id, shm_handle): (u64, u64),
        request: PreviewSize,
    ) -> Result<(), Errno> {
        let record = owned_window_mut(&mut self.windows, caller, window_id)?;
        if record.renders_pending.contains(&request) {
            return Err(Errno::AlreadyExists);
        }
        // Waited on like a full desktop: the held conclusion is what the
        // client will be told of next, once it drains its mailbox.
        if sink.holds_render(record.event_endpoint, window_id) {
            return Err(Errno::LimitExceeded);
        }
        record
            .renders_pending
            .try_reserve(1)
            .map_err(|_| Errno::OutOfMemory)?;
        host.preview_render_requested(
            app,
            window_id,
            ClientRegion::of(caller, shm_handle),
            request,
        )?;
        record.renders_pending.push(request);
        Ok(())
    }

    /// Relay `caller`'s tooltip declaration for its own window `window_id`.
    ///
    /// The window and the bounds are the engine's to check; the declaration
    /// itself is the host's to hold and act on. A window the caller does not
    /// own answers exactly like one that never existed, so a client learns
    /// nothing about another application's windows.
    fn set_tooltip(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        window_id: u64,
        region: WindowRegion,
        text: &str,
    ) -> Result<(), Errno> {
        if !self.owns(caller, window_id) {
            return Err(Errno::NotFound);
        }
        host.tooltip_declared(window_id, region, text)
    }

    /// Pop the oldest target queued for `caller`, or `None` once the queue is
    /// drained.
    ///
    /// Popping is what makes a target one-shot: there is no id to mint or
    /// validate, and the ordering is the protocol. An application with no
    /// queue at all — nothing was ever handed to it — answers like one whose
    /// queue is drained, so a pull leaks nothing about other clients.
    fn take_open_target(&mut self, caller: ProcId) -> Option<OpenEntry> {
        let queue = self.open_targets.get_mut(&caller)?;
        let entry = queue.pop_front();
        if queue.is_empty() {
            self.open_targets.remove(&caller);
        }
        entry
    }

    /// Where an application-scoped event reaches `app`: its declared
    /// icon-bar route, else the event endpoint of its most recent window.
    ///
    /// The bar route is preferred because an application that declared one
    /// receives every application-scoped event there, and it is the only
    /// route an instance with no window has. Falling back to a window keeps
    /// an application that opted out of the icon bar reachable — it is a
    /// window-owning client like any other, and a hand-over is not a bar
    /// gesture.
    fn app_event_endpoint(&self, app: ProcId) -> Option<u64> {
        if let Some(&endpoint) = self.app_bars.get(&app) {
            return Some(endpoint);
        }
        self.windows
            .iter()
            .rev()
            .find(|(_, record)| record.owner == app)
            .map(|(_, record)| record.event_endpoint)
    }

    /// Hand application `app` the target `make` produces, waking it with a
    /// [`WindowEvent::OpenRequested`] and what `activation` lets it bring
    /// forward.
    ///
    /// `make` runs only once `app` is known to take the entry — reachable,
    /// with room, and woken — because a document entry is a delegation the
    /// kernel cannot take back, and one minted for an instance that was then
    /// refused would sit in its table unannounced. The caller may therefore
    /// read a refusal as "the instance does not have it" and start a fresh
    /// process instead. An entry refused after the wake costs only that wake,
    /// which the application's pull answers as a drained queue.
    ///
    /// # Errors
    ///
    /// * [`Errno::NotFound`] — `app` has no route an event can reach it by,
    ///   so there is no live instance to hand anything to.
    /// * [`Errno::NoSpace`] — the application already holds
    ///   [`WINDOW_MAX_OPEN_TARGETS`] targets. The newest is refused with the
    ///   refusal stated rather than an older one dropped silently, or the
    ///   queue grown without bound.
    /// * Whatever the wake's delivery or `make` refused with.
    /// * [`Errno::LengthOutOfRange`] — an empty path or pane, or a path or
    ///   pane longer than the channel carries.
    /// * [`Errno::OutOfRange`] — a document naming no delegation.
    pub fn hand_over_open_target(
        &mut self,
        sink: &mut dyn EventSink,
        app: ProcId,
        activation: Activation,
        make: impl FnOnce() -> Result<OpenEntry, Errno>,
    ) -> Result<(), Errno> {
        let endpoint = self.app_event_endpoint(app).ok_or(Errno::NotFound)?;
        if self
            .open_targets
            .get(&app)
            .is_some_and(|queue| queue.len() >= WINDOW_MAX_OPEN_TARGETS)
        {
            return Err(Errno::NoSpace);
        }
        sink.deliver(endpoint, &WindowEvent::OpenRequested)?;
        let entry = make()?;
        match &entry {
            OpenEntry::Path(path) | OpenEntry::Image(path) => {
                if path.is_empty() || path.len() > tairix_abi::FS_PATH_MAX {
                    return Err(Errno::LengthOutOfRange);
                }
            }
            OpenEntry::Document { grant, .. } => {
                if *grant == 0 {
                    return Err(Errno::OutOfRange);
                }
            }
            // A file is placed only in a window of the application it is
            // queued for.
            OpenEntry::Placed {
                placement, grant, ..
            } => {
                if *grant == 0 || self.owner_of(placement.window_id) != Some(app) {
                    return Err(Errno::OutOfRange);
                }
            }
            OpenEntry::Pane(pane) => {
                if pane.is_empty() || pane.len() > tairix_abi::window_ipc::WINDOW_PANE_NAME_MAX {
                    return Err(Errno::LengthOutOfRange);
                }
            }
        }
        let queue = self.open_targets.entry(app).or_default();
        // A delegation handle is one-shot, and the kernel hands the *same*
        // handle back when the same authority is granted to the same process
        // twice. Queueing it twice would therefore promise a second document
        // the first pull consumes, so an entry already waiting is the answer.
        let waiting = entry
            .grant()
            .is_some_and(|grant| queue.iter().any(|held| held.grant() == Some(grant)));
        if !waiting {
            queue.push_back(entry);
        }
        self.mint(app, activation);
        Ok(())
    }

    fn mint(&mut self, app: ProcId, activation: Activation) {
        match activation {
            Activation::Granted { focus } => {
                self.activations.insert(app, focus);
            }
            Activation::Withheld => {
                self.activations.remove(&app);
            }
        }
    }

    /// Whether `caller` is the attested owner of live window `window_id`.
    fn owns(&self, caller: ProcId, window_id: u64) -> bool {
        self.windows
            .get(&window_id)
            .is_some_and(|record| record.owner == caller)
    }

    /// Accept a menu open for `caller`'s window `window_id`, named `open_id`,
    /// which its one outcome will name.
    ///
    /// At most one open is unanswered per window: while one is, a second is
    /// refused, which a well-behaved application cannot reach — its chain
    /// holds the seat's grab, so the press that would open another is
    /// consumed there. The host must accept before anything is recorded, so
    /// a refused open leaves no state.
    fn open_menu(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        (window_id, open_id): (u64, u64),
        anchor: WindowRegion,
        menu: &AppMenu,
    ) -> Result<(), Errno> {
        // Owned-window check first: a window the caller does not own
        // answers exactly like one that never existed.
        owned_window(&self.windows, caller, window_id)?;
        self.admit(caller, open_id)?;
        let record = owned_window_mut(&mut self.windows, caller, window_id)?;
        if record.menu_open.is_some() {
            return Err(Errno::AlreadyExists);
        }
        host.menu_open_requested(window_id, open_id, anchor, menu)?;
        record.menu_open = Some(open_id);
        // A commit the application never pulled belongs to the gesture that
        // is now over, so it goes with the open that replaces it.
        record.menu_text = None;
        Ok(())
    }

    /// Restate the rows of `caller`'s window `window_id`'s chain `open_id`,
    /// which must be the window's unanswered open. One just answered is
    /// accepted and restates nothing — the application asked before the
    /// answer reached it — and any other is refused.
    fn restate_menu(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        (window_id, open_id, revision): (u64, u64, u32),
        menu: &AppMenu,
    ) -> Result<(), Errno> {
        let record = owned_window(&self.windows, caller, window_id)?;
        if record.menu_open != Some(open_id) {
            return if record.menu_answered == Some(open_id) {
                Ok(())
            } else {
                Err(Errno::OutOfRange)
            };
        }
        host.menu_restate_requested(window_id, open_id, revision, menu)
    }

    /// Record the text the user committed into the quick-entry field of the
    /// chain opened as `open_id` on window `window_id`, for its application
    /// to pull.
    ///
    /// Called by the embedder as it settles the chain, *before* it delivers
    /// the `Entered` outcome, so the answer and the text it refers to are
    /// never out of order. The open must be the one that window is still
    /// waiting on: a text recorded against any other names a gesture whose
    /// answer has already gone, and is refused rather than held for a puller
    /// to find.
    ///
    /// # Errors
    ///
    /// * [`Errno::NotFound`] — no such live window.
    /// * [`Errno::OutOfRange`] — `open_id` is not the window's unanswered
    ///   open.
    /// * [`Errno::LengthOutOfRange`] — longer than a quick-entry field can
    ///   hold.
    pub fn record_menu_text(
        &mut self,
        window_id: u64,
        open_id: u64,
        text: &str,
    ) -> Result<(), Errno> {
        if text.len() > APP_MENU_ENTRY_MAX {
            return Err(Errno::LengthOutOfRange);
        }
        let record = self.windows.get_mut(&window_id).ok_or(Errno::NotFound)?;
        if record.menu_open != Some(open_id) {
            return Err(Errno::OutOfRange);
        }
        record.menu_text = Some(CommittedText {
            open_id,
            text: String::from(text),
        });
        Ok(())
    }

    /// Hold `text`, committed into the quick-entry field of application
    /// `client`'s icon-bar row `item` on the menu of its declaration
    /// `revision`, for its [`WindowRequest::TakeAppBarText`], in place of any
    /// it held for that row.
    ///
    /// # Errors
    ///
    /// * [`Errno::NotFound`] — `client` declared no icon-bar presence.
    /// * [`Errno::LengthOutOfRange`] — longer than a quick-entry field holds.
    /// * [`Errno::LimitExceeded`] — `client` already holds a text for every
    ///   row its declaration can carry.
    pub fn record_app_bar_text(
        &mut self,
        client: ProcId,
        item: AppMenuItemId,
        revision: u32,
        text: &str,
    ) -> Result<(), Errno> {
        if text.len() > APP_MENU_ENTRY_MAX {
            return Err(Errno::LengthOutOfRange);
        }
        if !self.app_bars.contains_key(&client) {
            return Err(Errno::NotFound);
        }
        let held = self.app_bar_texts.entry(client).or_default();
        if held.len() >= APP_MENU_MAX_TOTAL_ROWS && !held.contains_key(&item) {
            return Err(Errno::LimitExceeded);
        }
        held.insert(
            item,
            CommittedRowText {
                revision,
                text: String::from(text),
            },
        );
        Ok(())
    }

    /// Take the text `caller`'s icon-bar row `item` holds, if it was committed
    /// under `revision`: a commit under another stays held for the answer
    /// that names it.
    fn take_app_bar_text(
        &mut self,
        caller: ProcId,
        item: AppMenuItemId,
        revision: u32,
    ) -> Option<String> {
        let held = self.app_bar_texts.get_mut(&caller)?;
        if held.get(&item)?.revision != revision {
            return None;
        }
        let taken = held.remove(&item).map(|committed| committed.text);
        if held.is_empty() {
            self.app_bar_texts.remove(&caller);
        }
        taken
    }

    /// Take the text committed for `caller`'s window `window_id` under
    /// `open_id`, if that is the one it holds.
    ///
    /// Taken once: the slot is emptied, so a second pull answers nothing and
    /// two readers cannot both act on one commit. A window the caller does
    /// not own answers exactly like one that never existed.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] — no such live window owned by `caller`.
    fn take_menu_text(
        &mut self,
        caller: ProcId,
        window_id: u64,
        open_id: u64,
    ) -> Result<Option<String>, Errno> {
        let record = owned_window_mut(&mut self.windows, caller, window_id)?;
        if record.menu_text.as_ref().map(|held| held.open_id) != Some(open_id) {
            return Ok(None);
        }
        Ok(record.menu_text.take().map(|held| held.text))
    }

    /// Retitle `caller`'s window `window_id` to `title`.
    ///
    /// Ownership is checked before the host is told anything, so a
    /// retitle aimed at another client's window (or issued by a kernel
    /// caller, which owns none) answers `NotFound` and changes nothing.
    /// A host refusal leaves the previous title standing.
    fn set_title(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        window_id: u64,
        title: &str,
    ) -> Result<(), Errno> {
        owned_window(&self.windows, caller, window_id)?;
        host.window_retitled(window_id, title)
    }

    /// Restate `caller`'s window `window_id`'s resize range through the
    /// host.
    ///
    /// Ownership is checked before the host is told anything, exactly as a
    /// retitle is, so a range aimed at another client's window answers
    /// `NotFound` and changes nothing. A host refusal leaves the previous
    /// range standing.
    fn set_sizing(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        window_id: u64,
        sizing: WindowSizing,
    ) -> Result<(), Errno> {
        owned_window(&self.windows, caller, window_id)?;
        host.window_sizing_changed(window_id, sizing)
    }

    /// Ask the host to put `caller`'s window `window_id` into `state`.
    ///
    /// Ownership is checked before the host is told anything, exactly as a
    /// retitle is, so a state aimed at another client's window answers
    /// `NotFound` and changes nothing.
    fn set_size_state(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        window_id: u64,
        state: WindowSizeState,
    ) -> Result<(), Errno> {
        owned_window(&self.windows, caller, window_id)?;
        host.window_size_state_changed(window_id, state)
    }

    /// Record `caller`'s icon-bar declaration and hand it to the host.
    ///
    /// The route is remembered only once the host accepted the
    /// declaration, so a refused one leaves nothing behind to deliver a
    /// bar event to (fail closed), and a re-declaration replaces the
    /// previous route rather than accumulating one.
    fn set_app_bar(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        bar: &AppBar,
    ) -> Result<(), Errno> {
        host.app_bar_declared(caller, bar)?;
        self.app_bars.insert(caller, bar.event_endpoint);
        Ok(())
    }

    /// Set `caller`'s window `window_id`'s backdrop-blur radius. Always
    /// succeeds for an owned window: the radius was already bounded by the
    /// engine's ABI decode, so there is nothing left to fail once
    /// ownership holds.
    fn set_backdrop_blur(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        window_id: u64,
        radius_px: u16,
    ) -> Result<(), Errno> {
        owned_window(&self.windows, caller, window_id)?;
        host.backdrop_blur_set(window_id, radius_px);
        Ok(())
    }

    /// Close `caller`'s window `window_id`, dropping its region mapping.
    ///
    /// Closing a top-level window also closes every transient hung from it,
    /// so a menu, palette or dialog can never outlive the window it belongs
    /// to. Closing a transient's own id tears down only that transient — a
    /// transient has none of its own. An alert holding any window closed is
    /// dismissed with it.
    fn close(
        &mut self,
        host: &mut dyn WindowHost,
        sink: &mut dyn EventSink,
        caller: ProcId,
        window_id: u64,
    ) -> Result<(), Errno> {
        owned_window(&self.windows, caller, window_id)?;
        let closed = self.remove_with_transients(host, window_id);
        let held: Vec<u64> = self
            .alerts
            .iter()
            .filter(|(_, record)| record.window.is_some_and(|held| closed.contains(&held)))
            .map(|(&id, _)| id)
            .collect();
        for alert_id in held {
            host.alert_withdrawn(alert_id);
            // The alert is gone whether or not its answer could be sent.
            let _ = self.dismiss_alert(sink, alert_id);
        }
        Ok(())
    }

    /// Accept an alert for `caller`, named `alert_id`, which its one answer
    /// names.
    ///
    /// The owner, when named, must be one of the caller's own top-level
    /// windows or dialogs: a popup or tool window comes and goes with a
    /// gesture, and a layer surface never takes the keyboard an answer is
    /// typed with. The host
    /// must accept before anything is recorded.
    fn open_alert(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        alert_id: u64,
        (event_endpoint, owner_window_id): (u64, u64),
        spec: &AlertSpec,
    ) -> Result<(), Errno> {
        if caller.is_kernel() {
            return Err(Errno::PermissionDenied);
        }
        self.admit(caller, alert_id)?;
        let window = match owner_window_id {
            0 => None,
            id => {
                let record = owned_window(&self.windows, caller, id)?;
                let fleeting = record
                    .transient
                    .is_some_and(|transient| transient.kind != TransientKind::Dialog);
                if fleeting || record.layer.is_some() {
                    return Err(Errno::OutOfRange);
                }
                Some(id)
            }
        };
        let open = self
            .alerts
            .values()
            .filter(|record| record.owner == caller)
            .count();
        if open >= ALERT_MAX_OPEN {
            return Err(Errno::LimitExceeded);
        }
        let focus = self.holds_the_front(host, caller);
        host.alert_opened(&AlertOpen {
            alert_id,
            owner: caller,
            window,
            focus,
            spec,
        })?;
        if focus {
            self.activations.remove(&caller);
        }
        self.alerts.insert(
            alert_id,
            AlertRecord {
                owner: caller,
                event_endpoint,
                window,
                buttons: spec.button_count(),
            },
        );
        Ok(())
    }

    /// Withdraw `caller`'s own alert `alert_id`, answering it `Dismissed`.
    ///
    /// An alert the caller does not own answers exactly like one that never
    /// existed.
    fn close_alert(
        &mut self,
        host: &mut dyn WindowHost,
        sink: &mut dyn EventSink,
        caller: ProcId,
        alert_id: u64,
    ) -> Result<(), Errno> {
        if self
            .alerts
            .get(&alert_id)
            .is_none_or(|record| record.owner != caller)
        {
            return Err(Errno::NotFound);
        }
        host.alert_withdrawn(alert_id);
        self.dismiss_alert(sink, alert_id)
    }

    /// Answer alert `alert_id` with the user's choice of its button `index`,
    /// and forget it: each alert is answered exactly once. The answer is the
    /// user's gesture toward its application, so it lends what `activation`
    /// allows — the question that follows, or the window it is about,
    /// brought forward.
    ///
    /// The host calls this once it has taken the alert's window down.
    ///
    /// # Errors
    ///
    /// * [`Errno::NotFound`] — no such alert is up (already answered, or its
    ///   application is gone).
    /// * [`Errno::OutOfRange`] — a button the alert does not offer. Its
    ///   window being down, it is answered as dismissed and forgotten, never
    ///   answered as a choice it could not have been given.
    /// * Any [`Errno`] the sink surfaces. The alert is forgotten all the same:
    ///   its window is down, so nothing could answer it again.
    pub fn answer_alert(
        &mut self,
        sink: &mut dyn EventSink,
        alert_id: u64,
        index: u8,
        activation: Activation,
    ) -> Result<(), Errno> {
        let record = self.alerts.get(&alert_id).ok_or(Errno::NotFound)?;
        if usize::from(index) >= record.buttons {
            let _ = self.dismiss_alert(sink, alert_id);
            return Err(Errno::OutOfRange);
        }
        let record = self.alerts.remove(&alert_id).ok_or(Errno::NotFound)?;
        let outcome = AlertOutcome::Chosen { index };
        sink.deliver(
            record.event_endpoint,
            &WindowEvent::AlertClosed { alert_id, outcome },
        )?;
        self.mint(record.owner, activation);
        Ok(())
    }

    /// Take alert `alert_id` down unanswered, telling its application so.
    fn dismiss_alert(&mut self, sink: &mut dyn EventSink, alert_id: u64) -> Result<(), Errno> {
        let record = self.alerts.remove(&alert_id).ok_or(Errno::NotFound)?;
        sink.deliver(
            record.event_endpoint,
            &WindowEvent::AlertClosed {
                alert_id,
                outcome: AlertOutcome::Dismissed,
            },
        )
    }

    /// Remove `window_id` and every transient keyed to it, telling the host
    /// of each teardown, and answer every id removed. The window is dropped
    /// first, then its transients, and a present held behind any of them is
    /// answered.
    fn remove_with_transients(&mut self, host: &mut dyn WindowHost, window_id: u64) -> Vec<u64> {
        let mut closed = Vec::from([window_id]);
        closed.extend(
            self.windows
                .iter()
                .filter(|(_, record)| {
                    record
                        .transient
                        .is_some_and(|transient| transient.parent == window_id)
                })
                .map(|(&id, _)| id),
        );
        for &id in &closed {
            if let Some(mut record) = self.windows.remove(&id) {
                self.unshown.count_out(&record.pending);
                self.answers.release(&mut record.pending, record.owner);
                end_landings(host, id, &record.landings);
            }
            host.window_closed(id);
        }
        closed
    }

    /// Unmap window `window_id`'s frame region, keeping the window itself:
    /// the bytes given back, or zero if it holds none (or the id is unknown).
    ///
    /// The session calls this for a window whose content it released while
    /// nobody could see it, and tells the client the same
    /// ([`WindowEvent::ContentReleased`]). Both sides have to let go for the
    /// physical pages to be freed at all — a mapping either side keeps holds
    /// every page of them — so releasing here without telling the client
    /// frees only address space, and telling the client without releasing
    /// here frees nothing.
    ///
    /// Everything else about the window survives: its geometry, owner, event
    /// route, title, and place in the stack. The client re-attaches a fresh
    /// region with an ordinary [`WindowRequest::Resize`], which is what its
    /// next paint does; until then a present is refused
    /// ([`Errno::NotAttached`]) rather than reading a mapping nobody has. A
    /// present held behind the frame it drops is answered: it was recorded.
    pub fn release_frames(&mut self, window_id: u64) -> u64 {
        let Some(record) = self.windows.get_mut(&window_id) else {
            return 0;
        };
        let bytes = record.mapped_bytes();
        // Dropping the region is the unmap: the mapper owns that side.
        record.region = None;
        self.unshown.count_out(&record.pending);
        record.pending.clear();
        self.answers.release(&mut record.pending, record.owner);
        bytes
    }

    /// Tear down every window `client` owns: the session calls this when
    /// the owning app exits, so no mapping or taskbar entry outlives its
    /// process.
    pub fn client_exited(&mut self, host: &mut dyn WindowHost, client: ProcId) {
        let closed: alloc::vec::Vec<u64> = self
            .windows
            .iter()
            .filter(|(_, record)| record.owner == client)
            .map(|(&id, _)| id)
            .collect();
        for id in closed {
            if let Some(record) = self.windows.remove(&id) {
                self.unshown.count_out(&record.pending);
                self.answers.drop_held(&record.pending);
                end_landings(host, id, &record.landings);
            }
            host.window_closed(id);
        }
        // Its calls ended with it, so nothing it was owed has anyone to take it.
        self.answers.forget(client);
        if self.ranges_held.remove(&client).is_some() {
            self.ranges.retain(|_, (owner, _)| *owner != client);
        }
        if self.app_bars.remove(&client).is_some() {
            host.app_bar_withdrawn(client);
        }
        self.app_bar_texts.remove(&client);
        self.slot_authorisations.remove(&client);
        // A target queued for a client that has gone is reachable by
        // nothing; its delegation dies with the process it was minted to.
        self.open_targets.remove(&client);
        self.activations.remove(&client);
        // Nobody is left to answer: each alert just comes down.
        let alerts: Vec<u64> = self
            .alerts
            .iter()
            .filter(|(_, record)| record.owner == client)
            .map(|(&id, _)| id)
            .collect();
        for alert_id in alerts {
            self.alerts.remove(&alert_id);
            host.alert_withdrawn(alert_id);
        }
    }

    /// Route one application-scoped event — an icon-bar click or menu
    /// outcome — to the application that declared the bar presence it
    /// belongs to, with what `activation` lets it bring forward.
    ///
    /// The destination comes from the declaration the engine recorded, not
    /// from the event, so the session cannot address a bar event to a
    /// process that never asked to be on the bar.
    ///
    /// # Errors
    ///
    /// * [`Errno::OutOfRange`] — `event` is window-scoped; those go
    ///   through [`Self::deliver_event`].
    /// * [`Errno::NotFound`] — `app` declared no icon-bar presence (it
    ///   never did, or it has exited); the session drops the event.
    /// * Any [`Errno`] the sink surfaces.
    pub fn deliver_app_event(
        &mut self,
        sink: &mut dyn EventSink,
        app: ProcId,
        event: &WindowEvent,
        activation: Activation,
    ) -> Result<(), Errno> {
        if event.window_id().is_some() {
            return Err(Errno::OutOfRange);
        }
        let endpoint = *self.app_bars.get(&app).ok_or(Errno::NotFound)?;
        sink.deliver(endpoint, event)?;
        self.mint(app, activation);
        Ok(())
    }

    /// Conclude window `window_id`'s pending pick: the file the user chose —
    /// the folder, for a subtree pick — or `None` when they chose nothing.
    ///
    /// Delivers `FilePicked`, `SubtreePicked` or `PickCancelled` and, once the
    /// sink accepted it, clears the pending pick and holds a chosen name for
    /// the owner's `TakePickedName`. The one way a pick concludes, so exactly one
    /// conclusion follows each accepted `PickFile` and a conclusion no one
    /// asked for is refused, not delivered.
    ///
    /// # Errors
    ///
    /// * [`Errno::NotFound`] — no such window.
    /// * [`Errno::OutOfRange`] — no pick is pending on it, a zero handle, or
    ///   one file for a pick that asked for a folder's files.
    /// * Any [`Errno`] the sink surfaces; the pick is still owed.
    pub fn conclude_pick(
        &mut self,
        sink: &mut dyn EventSink,
        window_id: u64,
        chosen: Option<PickedFile<'_>>,
    ) -> Result<(), Errno> {
        let record = self.windows.get_mut(&window_id).ok_or(Errno::NotFound)?;
        let owed = match record.pick_pending {
            None => false,
            Some(PickPurpose::Folder) => chosen.is_none(),
            Some(
                PickPurpose::Open
                | PickPurpose::Import
                | PickPurpose::Subtree
                | PickPurpose::Save { .. },
            ) => true,
        };
        if !owed || chosen.is_some_and(|file| file.handle == 0) {
            return Err(Errno::OutOfRange);
        }
        let event = match chosen {
            Some(folder) if record.pick_pending == Some(PickPurpose::Subtree) => {
                WindowEvent::SubtreePicked {
                    window_id,
                    handle: folder.handle,
                    writable: folder.writable,
                }
            }
            Some(file) => WindowEvent::FilePicked {
                window_id,
                handle: file.handle,
                writable: file.writable,
            },
            None => WindowEvent::PickCancelled { window_id },
        };
        sink.deliver(record.event_endpoint, &event)?;
        record.pick_pending = None;
        record.picked_name = chosen.map(|file| *file.name);
        Ok(())
    }

    /// Conclude window `window_id`'s pending folder pick with the `files` the
    /// session delegated and the number `left_out` past the pick's bound.
    ///
    /// Delivers `FolderPicked` and, once the sink accepted it, holds the files
    /// for the owner to take one at a time (`TakePickedFile`).
    ///
    /// # Errors
    ///
    /// * [`Errno::NotFound`] — no such window.
    /// * [`Errno::OutOfRange`] — no folder pick is pending on it, more files
    ///   than [`WINDOW_FOLDER_PICK_MAX`], or a zero handle.
    /// * Any [`Errno`] the sink surfaces; the pick is still owed.
    pub fn conclude_folder_pick(
        &mut self,
        sink: &mut dyn EventSink,
        window_id: u64,
        files: VecDeque<tairix_abi::window_ipc::PickedFile>,
        left_out: u32,
    ) -> Result<(), Errno> {
        let record = self.windows.get_mut(&window_id).ok_or(Errno::NotFound)?;
        if record.pick_pending != Some(PickPurpose::Folder)
            || files.len() > WINDOW_FOLDER_PICK_MAX
            || files.iter().any(|file| file.handle == 0)
        {
            return Err(Errno::OutOfRange);
        }
        let event = WindowEvent::FolderPicked {
            window_id,
            files: u32::try_from(files.len()).map_err(|_| Errno::OutOfRange)?,
            left_out,
        };
        sink.deliver(record.event_endpoint, &event)?;
        record.pick_pending = None;
        record.picked_files = files;
        Ok(())
    }

    /// Conclude the authorisation pending for `asker` with `outcome`,
    /// holding `output` — exactly what a captured task printed, and nothing
    /// for any other outcome — for the owner's `TakeAuthorisedOutput`.
    ///
    /// The one way an authorisation concludes, so exactly one `Authorised`
    /// follows each accepted `Authorise` and a conclusion no one asked for is
    /// refused, not delivered.
    ///
    /// # Errors
    ///
    /// * [`Errno::NotFound`] — no such window, or no such slot.
    /// * [`Errno::OutOfRange`] — no authorisation is pending there, or an
    ///   output that is not the one `outcome` states.
    /// * Any [`Errno`] the sink surfaces; the authorisation is still owed.
    pub fn conclude_authorisation(
        &mut self,
        sink: &mut dyn EventSink,
        asker: AuthoriseFor,
        outcome: AuthorisedOutcome,
        output: Vec<u8>,
    ) -> Result<(), Errno> {
        let captured = match outcome {
            AuthorisedOutcome::Captured { len, .. } => {
                if usize::try_from(len).ok() != Some(output.len()) {
                    return Err(Errno::OutOfRange);
                }
                true
            }
            AuthorisedOutcome::Ran { .. }
            | AuthorisedOutcome::Launched
            | AuthorisedOutcome::Overran { .. }
            | AuthorisedOutcome::Declined => {
                if !output.is_empty() {
                    return Err(Errno::OutOfRange);
                }
                false
            }
        };
        match asker {
            AuthoriseFor::Window(window_id) => {
                let record = self.windows.get_mut(&window_id).ok_or(Errno::NotFound)?;
                if !record.authorising {
                    return Err(Errno::OutOfRange);
                }
                sink.deliver(
                    record.event_endpoint,
                    &WindowEvent::Authorised {
                        window_id: Some(window_id),
                        outcome,
                    },
                )?;
                record.authorising = false;
                record.authorised_output = captured.then_some(output);
            }
            AuthoriseFor::Slot(app) => {
                let endpoint = *self.app_bars.get(&app).ok_or(Errno::NotFound)?;
                let slot = self
                    .slot_authorisations
                    .get_mut(&app)
                    .filter(|slot| slot.pending)
                    .ok_or(Errno::OutOfRange)?;
                sink.deliver(
                    endpoint,
                    &WindowEvent::Authorised {
                        window_id: None,
                        outcome,
                    },
                )?;
                slot.pending = false;
                slot.output = captured.then_some(output);
                if slot.output.is_none() {
                    self.slot_authorisations.remove(&app);
                }
            }
        }
        Ok(())
    }

    /// Tell the owner of window `window_id`, whose drag is carried, where it
    /// now is: `report` numbered `serial`, with `shift` held or not.
    ///
    /// A report over a window is addressed to that window, which must be the
    /// owner's own and the point inside it; any other is addressed to the
    /// window the drag began in. A desktop report holds `folder` for the
    /// owner to read ([`WindowRequest::QueryDragSpot`]) before it is sent,
    /// for every report of the one place: one that follows the last and names
    /// its folder goes on with it, so a report a later one superseded there is
    /// still read.
    ///
    /// # Errors
    ///
    /// * [`Errno::NotFound`] — no such window, or no drag is carried from it.
    /// * [`Errno::PermissionDenied`] — the window reported over is another
    ///   owner's.
    /// * [`Errno::OutOfRange`] — the point is outside that window.
    /// * Any [`Errno`] the sink surfaces.
    pub fn report_drag(
        &mut self,
        sink: &mut dyn EventSink,
        window_id: u64,
        serial: u32,
        report: DragReport<'_>,
        shift: bool,
    ) -> Result<(), Errno> {
        let source = self.windows.get(&window_id).ok_or(Errno::NotFound)?;
        if !source.drag_pending {
            return Err(Errno::NotFound);
        }
        let (owner, endpoint) = (source.owner, source.event_endpoint);
        let (addressed, at) = match report {
            DragReport::Window {
                window_id: over,
                x,
                y,
            } => {
                let record = self.windows.get(&over).ok_or(Errno::NotFound)?;
                if record.owner != owner {
                    return Err(Errno::PermissionDenied);
                }
                if x >= record.surface.width_px || y >= record.surface.height_px {
                    return Err(Errno::OutOfRange);
                }
                (over, DragAt::Window { x, y })
            }
            DragReport::Desktop { folder } => {
                let record = self.windows.get_mut(&window_id).ok_or(Errno::NotFound)?;
                hold_spot(&mut record.drag_spot, serial, folder);
                (window_id, DragAt::Desktop)
            }
            DragReport::Nowhere => (window_id, DragAt::Nowhere),
        };
        sink.deliver(
            endpoint,
            &WindowEvent::DragOver {
                window_id: addressed,
                serial,
                at,
                shift,
            },
        )
    }

    /// Conclude window `window_id`'s pending drag where `ended` says.
    ///
    /// Delivers `DragEnded` and, once the sink accepted it, clears the pending
    /// drag and holds an application target for the owner's `TakeDropTarget`
    /// — the one way a drag ends. A drop on the desktop names the folder held
    /// for the place the report numbered `serial` was made at, which stays for
    /// the owner to read, and lands as the owner steps it
    /// ([`WindowRequest::DropLanding`]). Answers the serial of a landing it
    /// ended past [`DROP_LANDINGS_MAX`], which the host ends as though its
    /// owner had.
    ///
    /// # Errors
    ///
    /// * [`Errno::NotFound`] — no such window.
    /// * [`Errno::OutOfRange`] — no drag is pending on it, a window drop names
    ///   a window that is not the owner's, or a desktop drop names a report
    ///   that held no folder.
    /// * Any [`Errno`] the sink surfaces; the conclusion is still owed.
    pub fn conclude_drag(
        &mut self,
        sink: &mut dyn EventSink,
        window_id: u64,
        ended: DragConclusion,
    ) -> Result<Option<u32>, Errno> {
        let record = self.windows.get(&window_id).ok_or(Errno::NotFound)?;
        if !record.drag_pending {
            return Err(Errno::OutOfRange);
        }
        let owner = record.owner;
        let (site, held) = match ended {
            DragConclusion::Nothing => (DropSite::Nothing, None),
            DragConclusion::Application(target) => {
                (DropSite::Application, Some(DropHeld::Target(target)))
            }
            DragConclusion::Placed(placement) => {
                // Only another application's window takes a placed file.
                match self.owner_of(placement.window_id) {
                    Some(into) if into != owner => {}
                    _ => return Err(Errno::OutOfRange),
                }
                (DropSite::Placed, Some(DropHeld::Placed(placement)))
            }
            DragConclusion::Window {
                window_id: over,
                x,
                y,
                operation,
            } => {
                if self.owner_of(over) != Some(owner) {
                    return Err(Errno::OutOfRange);
                }
                (
                    DropSite::Window {
                        window_id: over,
                        x,
                        y,
                        operation,
                    },
                    None,
                )
            }
            DragConclusion::Desktop { serial, operation } => {
                let held = record
                    .drag_spot
                    .as_ref()
                    .is_some_and(|spot| spot.holds(serial));
                if !held {
                    return Err(Errno::OutOfRange);
                }
                (DropSite::Desktop { serial, operation }, None)
            }
        };
        let record = self.windows.get_mut(&window_id).ok_or(Errno::NotFound)?;
        sink.deliver(
            record.event_endpoint,
            &WindowEvent::DragEnded { window_id, site },
        )?;
        record.drag_pending = false;
        record.dropped = held;
        let DropSite::Desktop { serial, .. } = site else {
            return Ok(None);
        };
        Ok(land_drop(&mut record.landings, serial, record.drag_count))
    }

    /// Route one event to the owning app of the window it addresses:
    /// validate it against the live window, encode it, and hand it to
    /// `sink` for the window's event endpoint.
    ///
    /// A pick, drag or authorisation conclusion is refused: each goes through
    /// its own conclusion ([`Self::conclude_pick`], [`Self::conclude_drag`],
    /// [`Self::conclude_authorisation`]), which holds what it ended with.
    ///
    /// A `MenuClosed` outcome must name the window's *own* unanswered open:
    /// an open that was never accepted, one already answered, or another
    /// window's, is refused. The outcome concludes it, which is what makes
    /// exactly-once a property rather than a convention: a second outcome
    /// for one open cannot be delivered, and one gesture's dismissal can
    /// never arrive as another's answer. A `MenuChosen` is held to the same
    /// open and concludes nothing, so any number may precede the answer and
    /// none may follow it.
    ///
    /// # Errors
    ///
    /// * [`Errno::NotFound`] — no such window (it was closed, or never
    ///   existed); the session drops the event.
    /// * [`Errno::OutOfRange`] — a pointer event, a save offer, or a save
    ///   offer's drop point outside the window's surface; a pick or an
    ///   authorisation conclusion, or a drag report, each of which has its
    ///   own route; a preview no render of the window's awaits; an event
    ///   naming anything but the window's unanswered open (routing bugs,
    ///   refused rather than delivered); or an application-scoped event
    ///   (those go through [`Self::deliver_app_event`]).
    /// * Any [`Errno`] the sink surfaces; a refused delivery leaves an open
    ///   still owed (the session decides whether to retry or tear the client
    ///   down). A sink that *accepts* it is answering for it, whether it goes
    ///   out now or from a hold-back.
    pub fn deliver_event(
        &mut self,
        sink: &mut dyn EventSink,
        event: &WindowEvent,
    ) -> Result<(), Errno> {
        let window_id = event.window_id().ok_or(Errno::OutOfRange)?;
        let record = self.windows.get(&window_id).ok_or(Errno::NotFound)?;
        if let WindowEvent::Pointer { x, y, .. }
        | WindowEvent::SaveOffered { x, y, .. }
        | WindowEvent::SaveOfferEnded {
            dropped: Some((x, y)),
            ..
        }
        | WindowEvent::ItemsOffered { x, y, .. }
        | WindowEvent::FileOffered { x, y, .. }
        | WindowEvent::ItemsOfferEnded {
            dropped: Some(OfferDrop { x, y, .. }),
            ..
        } = *event
        {
            if x >= record.surface.width_px || y >= record.surface.height_px {
                return Err(Errno::OutOfRange);
            }
        }
        if matches!(
            event,
            WindowEvent::FilePicked { .. }
                | WindowEvent::PickCancelled { .. }
                | WindowEvent::FolderPicked { .. }
                | WindowEvent::DragOver { .. }
                | WindowEvent::DragEnded { .. }
                | WindowEvent::Authorised { .. }
        ) {
            return Err(Errno::OutOfRange);
        }
        let concludes_render = match *event {
            WindowEvent::PreviewRendered {
                subject,
                width,
                height,
                ..
            } => {
                let render = PreviewSize {
                    subject,
                    width,
                    height,
                };
                let owed = record
                    .renders_pending
                    .iter()
                    .position(|pending| *pending == render)
                    .ok_or(Errno::OutOfRange)?;
                Some(owed)
            }
            _ => None,
        };
        let names_open = match *event {
            WindowEvent::MenuClosed { open_id, .. } | WindowEvent::MenuChosen { open_id, .. } => {
                Some(open_id)
            }
            _ => None,
        };
        if names_open.is_some() && names_open != record.menu_open {
            return Err(Errno::OutOfRange);
        }
        let concludes_open = matches!(*event, WindowEvent::MenuClosed { .. });
        let endpoint = record.event_endpoint;
        sink.deliver(endpoint, event)?;
        if let Some(record) = self.windows.get_mut(&window_id) {
            if let Some(owed) = concludes_render {
                record.renders_pending.swap_remove(owed);
            }
            if concludes_open {
                record.menu_answered = record.menu_open.take();
            }
        }
        if matches!(*event, WindowEvent::Focus { focused: true, .. }) {
            self.activations.clear();
        }
        Ok(())
    }

    /// Raise and focus `caller`'s own window `window_id` on its request:
    /// under an activation minted while the keyboard was where it still is,
    /// which the request spends, or while one of the caller's windows already
    /// holds the keyboard.
    ///
    /// # Errors
    ///
    /// [`Errno::NotFound`] for a window the caller does not own,
    /// [`Errno::PermissionDenied`] for a caller with neither a live activation
    /// nor the keyboard, and the host's refusal to raise.
    fn activate(
        &mut self,
        host: &mut dyn WindowHost,
        caller: ProcId,
        window_id: u64,
    ) -> Result<(), Errno> {
        if !self.owns(caller, window_id) {
            return Err(Errno::NotFound);
        }
        let earned = self.holds_the_front(host, caller);
        self.activations.remove(&caller);
        if !earned {
            host.raise_refused(caller, window_id);
            return Err(Errno::PermissionDenied);
        }
        host.raise_requested(window_id)
    }

    /// Whether `caller` may bring a surface of its own over the user's work:
    /// one of its windows holds the keyboard, or it holds an activation minted
    /// while the keyboard was where it still is.
    fn holds_the_front(&self, host: &dyn WindowHost, caller: ProcId) -> bool {
        let focused = host.focused_window();
        let activated = self
            .activations
            .get(&caller)
            .is_some_and(|minted| *minted == focused);
        activated || focused.is_some_and(|focused| self.owns(caller, focused))
    }
}

/// The resize a decoded [`WindowRequest::Resize`] asks for, or `None` for any
/// other request.
fn resize_request(request: &WindowRequest) -> Option<ResizeSpec> {
    let WindowRequest::Resize {
        window_id,
        shm_handle,
        frame_count,
        width_px,
        height_px,
        stride_bytes,
        format,
    } = *request
    else {
        return None;
    };
    Some(ResizeSpec {
        window_id,
        shm_handle,
        frame_count,
        surface: surface_of(width_px, height_px, stride_bytes, format),
    })
}

/// The window and granted region a decoded [`WindowRequest::RenderPreview`]
/// renders into, and the picture it asks for, or `None` for any other
/// request.
fn preview_request(request: &WindowRequest) -> Option<((u64, u64), PreviewSize)> {
    let WindowRequest::RenderPreview {
        window_id,
        shm_handle,
        subject,
        width,
        height,
    } = *request
    else {
        return None;
    };
    Some((
        (window_id, shm_handle),
        PreviewSize {
            subject,
            width,
            height,
        },
    ))
}

/// The [`LayerSpec`] a decoded [`WindowRequest::OpenLayer`] describes, or
/// `None` for any other request.
fn layer_spec(request: &WindowRequest) -> Option<LayerSpec> {
    let WindowRequest::OpenLayer {
        window_id: _,
        shm_handle,
        event_endpoint,
        frame_count,
        width_px,
        height_px,
        stride_bytes,
        format,
        x,
        y,
        depth,
    } = *request
    else {
        return None;
    };
    Some(LayerSpec {
        shm_handle,
        event_endpoint,
        frame_count,
        surface: surface_of(width_px, height_px, stride_bytes, format),
        x,
        y,
        depth,
    })
}

/// Whether `request` is one of the operations `CAP_DESKTOP_LAYER` gates.
///
/// Named once, so the gate in `serve` and the refusal frame it writes can
/// never disagree about which operations are privileged.
fn is_layer_op(request: &WindowRequest) -> bool {
    matches!(
        request,
        WindowRequest::OpenLayer { .. }
            | WindowRequest::PlaceLayer { .. }
            | WindowRequest::TakeTerrain { .. }
    )
}

/// Look up `window_id` **as owned by** `caller`. A window owned by
/// someone else answers exactly like a window that does not exist, so
/// the reply leaks nothing about other clients.
fn owned_window<R>(
    windows: &BTreeMap<u64, WindowRecord<R>>,
    caller: ProcId,
    window_id: u64,
) -> Result<&WindowRecord<R>, Errno> {
    windows
        .get(&window_id)
        .filter(|record| record.owner == caller)
        .ok_or(Errno::NotFound)
}

/// [`owned_window`], to change.
fn owned_window_mut<R>(
    windows: &mut BTreeMap<u64, WindowRecord<R>>,
    caller: ProcId,
    window_id: u64,
) -> Result<&mut WindowRecord<R>, Errno> {
    windows
        .get_mut(&window_id)
        .filter(|record| record.owner == caller)
        .ok_or(Errno::NotFound)
}

/// Bytes one frame of `surface` occupies (`stride_bytes * height_px`),
/// refused if it overflows the host's address width.
fn frame_bytes(surface: &DisplayMode) -> Result<usize, Errno> {
    u64::from(surface.stride_bytes)
        .checked_mul(u64::from(surface.height_px))
        .and_then(|bytes| usize::try_from(bytes).ok())
        .ok_or(Errno::LengthOutOfRange)
}

/// Write a status-only reply into `reply`, returning its length.
fn status(reply: &mut [u8; WINDOW_REPLY_MAX], result: Result<(), Errno>) -> usize {
    reply[..STATUS_REPLY_LEN].copy_from_slice(&encode_status_reply(result));
    STATUS_REPLY_LEN
}

/// Write a `HandOverLaunch` outcome into `reply`, answering its length.
fn hand_over_reply(
    reply: &mut [u8; WINDOW_REPLY_MAX],
    result: Result<HandOverOutcome, Errno>,
) -> usize {
    let frame = encode_hand_over_reply(result);
    reply[..frame.len()].copy_from_slice(&frame);
    frame.len()
}

/// Write a `TakeMenuText` outcome into `reply`, answering its length.
///
/// The frame is only as long as the answer: "nothing held" costs its header
/// rather than the widest name.
fn menu_text_reply(
    reply: &mut [u8; WINDOW_REPLY_MAX],
    result: Result<Option<&str>, Errno>,
) -> usize {
    let mut frame = [0u8; WINDOW_MENU_TEXT_REPLY_MAX];
    let len = encode_menu_text_reply(&mut frame, result);
    reply[..len].copy_from_slice(&frame[..len]);
    len
}

/// Write a `QueryDragSpot` outcome into `reply` in place, answering its
/// length.
fn drag_spot_reply(reply: &mut [u8; WINDOW_REPLY_MAX], result: Result<&str, Errno>) -> usize {
    match reply.first_chunk_mut::<WINDOW_DRAG_SPOT_REPLY_MAX>() {
        Some(frame) => encode_drag_spot_reply(frame, result),
        None => status(reply, Err(Errno::BufferTooSmall)),
    }
}

/// Write an `OfferedFile` outcome into `reply` in place, answering its length.
fn offered_file_reply(reply: &mut [u8; WINDOW_REPLY_MAX], result: Result<&str, Errno>) -> usize {
    match reply.first_chunk_mut::<WINDOW_OFFERED_FILE_REPLY_MAX>() {
        Some(frame) => encode_offered_file_reply(frame, result),
        None => status(reply, Err(Errno::BufferTooSmall)),
    }
}

/// Write an `OfferedItems` page into `reply` in place, answering its length.
fn offered_items_reply<'i>(
    reply: &mut [u8; WINDOW_REPLY_MAX],
    result: Result<(u32, impl IntoIterator<Item = OfferedItem<'i>>), Errno>,
) -> usize {
    match reply.first_chunk_mut::<WINDOW_OFFERED_ITEMS_REPLY_MAX>() {
        Some(frame) => encode_offered_items_reply(frame, result),
        None => status(reply, Err(Errno::BufferTooSmall)),
    }
}

/// Write a `TakePickedFile` outcome into `reply`, answering its length.
fn picked_file_reply(
    reply: &mut [u8; WINDOW_REPLY_MAX],
    result: Result<&tairix_abi::window_ipc::PickedFile, Errno>,
) -> usize {
    let mut frame = [0u8; WINDOW_PICKED_FILE_REPLY_MAX];
    let len = encode_picked_file_reply(&mut frame, result);
    reply[..len].copy_from_slice(&frame[..len]);
    len
}

/// Write a `TakeAuthorisedOutput` outcome into `reply`, answering its
/// length: the frame is encoded in place, so the widest output costs one
/// write rather than a second buffer and a copy.
fn authorised_output_reply(
    reply: &mut [u8; WINDOW_REPLY_MAX],
    result: Result<&[u8], Errno>,
) -> usize {
    match reply.first_chunk_mut::<WINDOW_AUTHORISED_OUTPUT_REPLY_MAX>() {
        Some(frame) => encode_authorised_output_reply(frame, result),
        None => status(reply, Err(Errno::BufferTooSmall)),
    }
}

/// Write a `TakePickedName` outcome into `reply`, answering its length.
fn picked_name_reply(
    reply: &mut [u8; WINDOW_REPLY_MAX],
    result: Result<&DocumentName, Errno>,
) -> usize {
    let mut frame = [0u8; WINDOW_PICKED_NAME_REPLY_MAX];
    let len = encode_picked_name_reply(&mut frame, result);
    reply[..len].copy_from_slice(&frame[..len]);
    len
}

/// Write a `TakeOpenTarget` outcome into `reply`, answering its length.
///
/// The frame is only as long as the answer: the drained queue costs its
/// header, not the widest path.
fn open_target_reply(
    reply: &mut [u8; WINDOW_REPLY_MAX],
    result: Result<Option<OpenTarget<'_>>, Errno>,
) -> usize {
    let mut frame = [0u8; WINDOW_OPEN_TARGET_REPLY_MAX];
    let len = encode_open_target_reply(&mut frame, result);
    reply[..len].copy_from_slice(&frame[..len]);
    len
}

/// Write one page of `host`'s wallpaper catalog from `from` into `reply`,
/// answering its length.
///
/// The page is as long as the entries it carried, not the widest one the
/// channel admits, and the total lets the caller decide whether to ask
/// again.
fn wallpapers_reply(
    reply: &mut [u8; WINDOW_REPLY_MAX],
    host: &mut dyn WindowHost,
    from: u16,
) -> usize {
    let catalog = host.wallpaper_catalog();
    let total = u16::try_from(catalog.len()).unwrap_or(u16::MAX);
    let page = catalog.get(usize::from(from)..).unwrap_or(&[]);
    let mut frame = [0u8; WINDOW_WALLPAPERS_REPLY_MAX];
    let len = encode_wallpapers_reply(
        &mut frame,
        Ok((
            total,
            page.iter().map(|name| WallpaperEntry {
                category: name.category.as_bytes(),
                file: name.file.as_bytes(),
            }),
        )),
    );
    reply[..len].copy_from_slice(&frame[..len]);
    len
}

/// Write `host`'s cursor-set choice space into `reply`, answering its
/// length.
///
/// The whole choice space, never a page: a store offers at most what one
/// reply frame holds, so a chooser learns every set it may offer in one
/// call.
fn cursor_sets_reply(reply: &mut [u8; WINDOW_REPLY_MAX], host: &mut dyn WindowHost) -> usize {
    let sets = host.cursor_sets();
    let mut frame = [0u8; WINDOW_CURSOR_SETS_REPLY_MAX];
    let len = encode_cursor_sets_reply(&mut frame, Ok(sets.iter().map(|set| set.0.as_bytes())));
    reply[..len].copy_from_slice(&frame[..len]);
    len
}

/// Write a cursor-set refusal into `reply`, answering its length.
fn cursor_sets_refusal(reply: &mut [u8; WINDOW_REPLY_MAX], err: Errno) -> usize {
    let mut frame = [0u8; WINDOW_CURSOR_SETS_REPLY_MAX];
    let len = encode_cursor_sets_reply(&mut frame, Err::<core::iter::Empty<&[u8]>, Errno>(err));
    reply[..len].copy_from_slice(&frame[..len]);
    len
}

/// Write the sources the host answered, or its refusal, into `reply`,
/// answering its length.
fn notify_sources_reply(
    reply: &mut [u8; WINDOW_REPLY_MAX],
    answered: Result<&[BundleId], Errno>,
) -> usize {
    let mut frame = [0u8; WINDOW_NOTIFY_SOURCES_REPLY_MAX];
    let len = encode_notify_sources_reply(
        &mut frame,
        answered.map(|sources| sources.iter().map(|source| source.as_str().as_bytes())),
    );
    reply[..len].copy_from_slice(&frame[..len]);
    len
}

/// Write a screensaver-catalogue refusal into `reply`, answering its length.
fn screensaver_refusal(reply: &mut [u8; WINDOW_REPLY_MAX], err: Errno) -> usize {
    let mut frame = [0u8; WINDOW_SCREENSAVER_REPLY_MAX];
    let len = encode_screensaver_reply(&mut frame, Err(err));
    reply[..len].copy_from_slice(&frame[..len]);
    len
}

/// Write a wallpaper-catalog refusal into `reply`, answering its length.
fn wallpapers_refusal(reply: &mut [u8; WINDOW_REPLY_MAX], err: Errno) -> usize {
    let mut frame = [0u8; WINDOW_WALLPAPERS_REPLY_MAX];
    let len = encode_wallpapers_reply(
        &mut frame,
        Err::<(u16, core::iter::Empty<WallpaperEntry<'_>>), Errno>(err),
    );
    reply[..len].copy_from_slice(&frame[..len]);
    len
}

/// Write a handle reply into `reply`, returning its length.
fn handle_reply(reply: &mut [u8; WINDOW_REPLY_MAX], result: Result<u64, Errno>) -> usize {
    reply[..WINDOW_HANDLE_REPLY_LEN].copy_from_slice(&encode_handle_reply(result));
    WINDOW_HANDLE_REPLY_LEN
}

/// Write a connect reply into `reply`, returning its length: the desktop,
/// this session's own attested identity — what an app requires of every
/// event's sender — and the caller's id range.
fn connect_reply(
    reply: &mut [u8; WINDOW_REPLY_MAX],
    result: Result<(DesktopInfo, ObjectIds), Errno>,
    server: ProcId,
) -> usize {
    reply[..WINDOW_CONNECT_REPLY_LEN].copy_from_slice(&encode_connect_reply(result, server));
    WINDOW_CONNECT_REPLY_LEN
}

/// The refusal `request` is owed for `err`, in the frame it would have been
/// answered with.
fn refused(
    request: &WindowRequest,
    reply: &mut [u8; WINDOW_REPLY_MAX],
    err: Errno,
    server: ProcId,
) -> usize {
    match request {
        WindowRequest::Connect => connect_reply(reply, Err(err), server),
        WindowRequest::TakeZoneStore => handle_reply(reply, Err(err)),
        _ => status(reply, Err(err)),
    }
}

/// The event mailbox `request` names for its answers, if it names one.
const fn named_mailbox(request: &WindowRequest) -> Option<u64> {
    match request {
        WindowRequest::Create { event_endpoint, .. }
        | WindowRequest::CreatePopup { event_endpoint, .. }
        | WindowRequest::CreateTool { event_endpoint, .. }
        | WindowRequest::CreateDialog { event_endpoint, .. }
        | WindowRequest::OpenLayer { event_endpoint, .. }
        | WindowRequest::OpenAlert { event_endpoint, .. } => Some(*event_endpoint),
        _ => None,
    }
}

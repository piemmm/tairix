//! Host tests: the server engine against mock seams, and the client
//! halves wired to a real [`WindowServer`] through a loopback transport,
//! so both halves are proven against the one shared definition of the
//! protocol semantics.

extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use tairix_abi::desktop::{Appearance, DesktopInfo};
use tairix_abi::driver::display::{
    DamageList, DamageRect, DisplayFormat, DisplayMode, RefreshRate,
};
use tairix_abi::input::{KeyInput, KeyValue, Modifiers, PhysicalKey, PointerButtonCode};
use tairix_abi::origin::{AppIdentity, ProcId, PROC_ID_LEN};
use tairix_abi::reply::decode_status_reply;
use tairix_abi::window_ipc::{
    AlertButton, AlertButtonRole, AlertOutcome, AlertSeverity, AlertSpec, AppBar, AppBarClick,
    AppMenu, AppMenuItem, AppMenuItemId, AppMenuLabel, AppMenuReason, AppMenuRow, AppMenuRowView,
    AppMenuSymbol, AuthorisedOutcome, BundleRunPath, ClipboardHeld, ClipboardKind, CursorShape,
    DialogModality, DocumentName, DragAt, DragItems, DropOperation, DropSite, DropTarget,
    HandOverDocument, HandOverOutcome, HandOverTarget, LayerDepth, MenuOutcome, MenuRefusal,
    PaneName, PickPurpose, PointerAction, PreviewOutcome, PreviewSubject, SaveFormats, SystemTask,
    TerrainPlate, TooltipText, WindowEvent, WindowRegion, WindowRequest, APP_MENU_ENTRY_MAX,
    APP_MENU_MAX_TOTAL_ROWS, DESKTOP_LAYER_MAX_PER_CLIENT, HAND_OVER_RUN_PATH_MAX,
    TOOLTIP_TEXT_MAX, WINDOW_FOLDER_PICK_MAX, WINDOW_MAX_OPEN_TARGETS, WINDOW_TITLE_MAX,
};
use tairix_abi::{BundleId, CapabilityId, Errno, PublisherId};
use tairix_display::{FrameRegion, ShmMapper};
use tairix_geometry::{Point, Rect, Region, Scale};

use tairix_abi::window_ipc::{LandingName, LandingOutcome, LandingStep, DROP_LANDINGS_MAX};

use crate::client::{
    pointer_point, present_damage, present_damage_list, presented_damage, retained_damage,
    DeclaredTip, EventDrain, EventError, EventSource, Owed, Parked, PostedRequest, Refused,
    Repaint, Target, WindowClient, WindowEvents, WindowTransport,
};
use crate::damage_in;
use crate::desktop::Desktop;
use crate::server::{
    client_frame_budget_bytes, Activation, AlertOpen, AuthoriseFor, CallerIdentity, ClientRegion,
    CursorSetName, DialogSpec, DragConclusion, DragReport, EventSink, FrameHost, HandOverDesk,
    LayerSpec, OpenEntry, PickedFile, PopupSpec, PreviewSize, Served, ToolSpec, TransientSpec,
    WallpaperName, WindowHost, WindowServer, WindowSizeState, WindowSizing, WINDOW_REPLY_MAX,
};

/// 4×3 BGRA test surface, stride == one scanline.
/// An activation minted while none of the engine's windows held the keyboard,
/// which is where the test host's keyboard starts.
const GRANTED: Activation = Activation::Granted { focus: None };

const SURFACE: DisplayMode = DisplayMode {
    width_px: 4,
    height_px: 3,
    stride_bytes: 16,
    format: DisplayFormat::Bgra8888,
};

/// Bytes one SURFACE frame occupies.
const FRAME_LEN: usize = 48;

/// The refusal the client reaped for a posted request, if it reaped one.
fn refusal<T: WindowTransport>(client: &mut WindowClient<T>) -> Option<Refused> {
    client.take_owed()?.err()
}

/// The refusal of `request` for `window_id` with `errno`.
const fn refused(window_id: u64, request: PostedRequest, errno: Errno) -> Refused {
    Refused {
        window_id,
        request,
        errno,
    }
}

/// The size the session kept for `window_id`, delivered again after it
/// refused a resize.
const fn kept(window_id: u64, mode: &DisplayMode) -> WindowEvent {
    WindowEvent::Resized {
        window_id,
        width_px: mode.width_px,
        height_px: mode.height_px,
        state: WindowSizeState::Restored,
    }
}

/// Window frames one test client may hold: eight [`SURFACE`] frames, so a
/// test can reach the bound in a handful of creates and prove the ninth is
/// refused. A real session derives this from the machine's RAM and its own
/// output ([`client_frame_budget_bytes`]).
const CLIENT_FRAME_MAX: u64 = 8 * FRAME_LEN as u64;

/// Frames a client may hold under [`CLIENT_FRAME_MAX`].
const FRAMES_PER_CLIENT: usize = 8;

/// The ticket the loopback presents for client A.
const TICKET_A: u64 = 1;
/// The ticket the loopback presents for client B.
const TICKET_B: u64 = 2;
/// A ticket attested as the kernel sentinel.
const TICKET_KERNEL: u64 = 8;
/// A ticket whose attestation fails.
const TICKET_UNATTESTED: u64 = 9;
/// Where the tickets the loopback mints, one per call, begin: clear of every
/// fixed ticket above.
const MINTED_TICKETS: u64 = 0x1_0000;

/// The event endpoint client A names in its creates.
const EVENTS_A: u64 = 0xA000;
/// The event endpoint client B names in its creates.
const EVENTS_B: u64 = 0xB000;

/// The serving session identity the loopback server stamps into every
/// successful create reply.
const SERVER: ProcId = ProcId::from_raw([0x5D; PROC_ID_LEN]);

/// A mapped region backed by a `Vec` with deterministic per-handle
/// content, so a presented frame slice is checkable byte for byte.
struct MockRegion(Vec<u8>);

impl FrameRegion for MockRegion {
    fn bytes(&self) -> &[u8] {
        &self.0
    }
}

/// A mapper over a fixed table of grant handle → region size.
struct MockMapper {
    /// Each region's length and, when a test binds it, the client that
    /// granted it: the kernel maps it for no other.
    regions: BTreeMap<u64, (usize, Option<ProcId>)>,
}

impl MockMapper {
    fn with_regions(regions: &[(u64, usize)]) -> Self {
        Self {
            regions: regions
                .iter()
                .map(|&(handle, len)| (handle, (len, None)))
                .collect(),
        }
    }

    /// Regions each granted by one client.
    fn granted(regions: &[(u64, usize, ProcId)]) -> Self {
        Self {
            regions: regions
                .iter()
                .map(|&(handle, len, grantor)| (handle, (len, Some(grantor))))
                .collect(),
        }
    }
}

/// The deterministic content byte at offset `i` of `handle`'s region.
fn region_byte(handle: u64, i: usize) -> u8 {
    handle.to_le_bytes()[0].wrapping_add(i.to_le_bytes()[0])
}

impl ShmMapper for MockMapper {
    type Region = MockRegion;

    fn map(&mut self, grantor: ProcId, handle: u64, min_len: usize) -> Result<MockRegion, Errno> {
        let &(len, granted_by) = self.regions.get(&handle).ok_or(Errno::NotFound)?;
        if granted_by.is_some_and(|granted_by| granted_by != grantor) {
            return Err(Errno::NotFound);
        }
        if len < min_len {
            return Err(Errno::LengthOutOfRange);
        }
        Ok(MockRegion(
            (0..len).map(|i| region_byte(handle, i)).collect(),
        ))
    }
}

/// The attestation table: each ticket maps to a fixed identity.
struct MockIdentity {
    /// Which tickets the kernel attests as holding `CAP_DESKTOP_LAYER`.
    /// Empty by default, so the gate is closed unless a test opens it.
    layer_holders: Vec<u64>,
    /// An attestation failure to surface instead of an answer.
    attest_error: Option<Errno>,
    /// The application each ticket is attested as running; absent means it
    /// runs no verified bundle.
    apps: Vec<(u64, AppIdentity)>,
    /// The call being served: the ticket minted for it, and the fixed ticket
    /// naming the identity it is attested as.
    minted: Option<(u64, u64)>,
}

fn proc_id(fill: u8) -> ProcId {
    ProcId::from_raw([fill; PROC_ID_LEN])
}

impl MockIdentity {
    /// An identity attesting `CAP_DESKTOP_LAYER` for each of `tickets`.
    fn holding_layer(tickets: &[u64]) -> Self {
        Self {
            layer_holders: tickets.to_vec(),
            attest_error: None,
            apps: Vec::new(),
            minted: None,
        }
    }

    /// The fixed ticket `ticket` attests: the identity a minted ticket was
    /// served under, or the ticket itself.
    fn attested(&self, ticket: u64) -> u64 {
        match self.minted {
            Some((minted, identity)) if minted == ticket => identity,
            _ => ticket,
        }
    }
}

impl CallerIdentity for MockIdentity {
    fn caller(&mut self, ticket: u64) -> Result<ProcId, Errno> {
        match self.attested(ticket) {
            TICKET_A => Ok(proc_id(0xA1)),
            TICKET_B => Ok(proc_id(0xB2)),
            TICKET_KERNEL => Ok(ProcId::KERNEL),
            _ => Err(Errno::NotFound),
        }
    }

    fn caller_mailbox(&mut self, ticket: u64) -> Result<u64, Errno> {
        match self.attested(ticket) {
            TICKET_A => Ok(EVENTS_A),
            TICKET_B => Ok(EVENTS_B),
            TICKET_KERNEL => Err(Errno::PermissionDenied),
            _ => Err(Errno::NotFound),
        }
    }

    fn caller_holds(&mut self, ticket: u64, cap: CapabilityId) -> Result<bool, Errno> {
        if let Some(err) = self.attest_error {
            return Err(err);
        }
        let ticket = self.attested(ticket);
        Ok(cap == CapabilityId::DESKTOP_LAYER && self.layer_holders.contains(&ticket))
    }

    fn caller_app(&mut self, ticket: u64) -> Result<Option<AppIdentity>, Errno> {
        if let Some(err) = self.attest_error {
            return Err(err);
        }
        let ticket = self.attested(ticket);
        Ok(self
            .apps
            .iter()
            .find(|(held, _)| *held == ticket)
            .map(|(_, app)| *app))
    }
}

/// A host recording every bridge call, optionally refusing opens, picker
/// requests.
struct RecordingHost {
    opened: Vec<(ProcId, u64, DisplayMode, String, WindowSizing)>,
    popups: Vec<(u64, u64, i32, i32, DisplayMode)>,
    /// Every tool window opened: its id, parent, carry, title and sizing.
    tools: Vec<(u64, u64, Option<u32>, String, WindowSizing)>,
    /// Every dialog opened: its id, owner, whether it earned the keyboard,
    /// how it holds its owner, and its title.
    dialogs: Vec<(u64, u64, bool, DialogModality, String)>,
    layers: Vec<(ProcId, u64, DisplayMode, i32, i32, LayerDepth)>,
    layer_places: Vec<(u64, i32, i32, LayerDepth)>,
    refuse_layer: Option<Errno>,
    /// The terrain the host reports, and the refusal it answers instead.
    terrain: Vec<TerrainPlate>,
    refuse_terrain: Option<Errno>,
    presented: Vec<(u64, Vec<u8>, DamageList)>,
    /// What a frame refused at the latch is refused with, if it is.
    refuse_frames: Option<Errno>,
    /// Every frame the latch could not show, by window.
    frames_refused: Vec<(u64, Errno)>,
    resized: Vec<(u64, DisplayMode)>,
    closed: Vec<u64>,
    picks: Vec<(u64, PickPurpose)>,
    /// Whether each pick the host was asked for earned the keyboard, and
    /// every pick the engine refused past a bound, as `(caller, window)`.
    pick_focus: Vec<bool>,
    picks_refused: Vec<(ProcId, u64)>,
    menu_opens: Vec<(u64, u64, WindowRegion, AppMenu)>,
    tooltips: Vec<(u64, WindowRegion, String)>,
    refuse_tooltip: Option<Errno>,
    cursors: Vec<(u64, CursorShape)>,
    /// Every window the engine asked the host to raise, and the window the
    /// host reports holding the keyboard.
    raised: Vec<u64>,
    focused: Option<u64>,
    /// Every raise the engine refused, as `(caller, window)`.
    raises_refused: Vec<(ProcId, u64)>,
    blur_sets: Vec<(u64, u16)>,
    retitled: Vec<(u64, String)>,
    resized_range: Vec<(u64, WindowSizing)>,
    size_states: Vec<(u64, WindowSizeState)>,
    refuse_size_state: Option<Errno>,
    app_bars: Vec<(ProcId, AppBar)>,
    app_bars_withdrawn: Vec<ProcId>,
    refuse_app_bar: Option<Errno>,
    refuse_open: bool,
    refuse_popup: bool,
    refuse_resize: Option<Errno>,
    refuse_retitle: Option<Errno>,
    refuse_sizing: Option<Errno>,
    refuse_pick: Option<Errno>,
    /// Every drag the host was asked to carry, and the refusal it answers
    /// instead.
    drags: Vec<(u64, String)>,
    refuse_drag: Option<Errno>,
    verdicts: Vec<(u64, u32, Option<DropOperation>)>,
    /// Every step of a drop onto the desktop the engine passed on.
    landings: Vec<(u64, u32, LandingStep)>,
    /// Every save offer answered, and every folder a dropped save was given,
    /// with the attested caller that gave it.
    save_answers: Vec<(u64, u32, bool)>,
    save_folders: Vec<(ProcId, u64, u32, u64)>,
    /// Every desktop items offer answered, and the items the host answers
    /// reads of, by window and drag.
    items_answers: Vec<(u64, u32, Option<DropOperation>)>,
    offered_items: Vec<(u64, u32, String, tairix_abi::fs::FileId)>,
    refuse_menu_open: Option<Errno>,
    /// Every alert put up, whether it earned the keyboard, and every one
    /// taken down, by id.
    alerts: Vec<(u64, ProcId, Option<u64>, AlertSpec)>,
    alert_focus: Vec<bool>,
    alerts_withdrawn: Vec<u64>,
    refuse_alert: Option<Errno>,
    hand_overs: Vec<(ProcId, String, HandOverTarget)>,
    /// Whether each hand-over found an activation its caller could spend.
    spent: Vec<bool>,
    /// What the host answers a hand-over with: `NotRunning` unless a test
    /// says otherwise, so the default is "nothing to reach".
    hand_over: Result<HandOverOutcome, Errno>,
    /// Every reveal asked for, as `(caller, window, location)`, and what the
    /// host answers one with.
    reveals: Vec<(ProcId, u64, u64)>,
    reveal: Result<(), Errno>,
    /// The desktop this host composites, or the refusal a host with no
    /// screen to describe answers with.
    desktop: Result<DesktopInfo, Errno>,
    /// The shipped wallpapers this host offers, and every render asked of
    /// it.
    wallpapers: Vec<WallpaperName>,
    renders: Vec<(u64, ClientRegion, PreviewSize)>,
    /// Who each render was attested as asking, where the engine asked.
    render_callers: Vec<Option<AppIdentity>>,
    refuse_render: Option<Errno>,
    /// Every clipboard set and get the engine passed on, by window.
    clipboard: Vec<(u64, ClientRegion)>,
    /// The cursor sets this host offers.
    cursor_sets: Vec<CursorSetName>,
    /// The sources this host answers, and the application every
    /// notify-source query and lock request was attributed to.
    notify_sources: Vec<BundleId>,
    asked_by: Vec<Option<AppIdentity>>,
    locks: usize,
    /// Every process the zone store was delegated to.
    zone_grants: Vec<ProcId>,
    /// Every screensaver preview document this host was handed.
    previews: Vec<String>,
    /// Every authorisation the engine passed on, and the refusal this host
    /// answers them with.
    authorisations: Vec<(Option<AppIdentity>, AuthoriseFor, SystemTask)>,
    refuse_authorise: Option<Errno>,
}

impl Default for RecordingHost {
    fn default() -> Self {
        Self {
            opened: Vec::new(),
            popups: Vec::new(),
            tools: Vec::new(),
            dialogs: Vec::new(),
            layers: Vec::new(),
            layer_places: Vec::new(),
            refuse_layer: None,
            terrain: Vec::new(),
            refuse_terrain: None,
            presented: Vec::new(),
            refuse_frames: None,
            frames_refused: Vec::new(),
            resized: Vec::new(),
            closed: Vec::new(),
            picks: Vec::new(),
            pick_focus: Vec::new(),
            picks_refused: Vec::new(),
            wallpapers: Vec::new(),
            renders: Vec::new(),
            render_callers: Vec::new(),
            refuse_render: None,
            clipboard: Vec::new(),
            cursor_sets: Vec::new(),
            notify_sources: Vec::new(),
            asked_by: Vec::new(),
            locks: 0,
            zone_grants: Vec::new(),
            previews: Vec::new(),
            authorisations: Vec::new(),
            refuse_authorise: None,
            menu_opens: Vec::new(),
            tooltips: Vec::new(),
            refuse_tooltip: None,
            cursors: Vec::new(),
            raised: Vec::new(),
            focused: None,
            raises_refused: Vec::new(),
            blur_sets: Vec::new(),
            retitled: Vec::new(),
            resized_range: Vec::new(),
            size_states: Vec::new(),
            refuse_size_state: None,
            app_bars: Vec::new(),
            app_bars_withdrawn: Vec::new(),
            refuse_app_bar: None,
            refuse_open: false,
            refuse_popup: false,
            refuse_resize: None,
            refuse_retitle: None,
            refuse_sizing: None,
            refuse_pick: None,
            drags: Vec::new(),
            refuse_drag: None,
            verdicts: Vec::new(),
            landings: Vec::new(),
            save_answers: Vec::new(),
            save_folders: Vec::new(),
            items_answers: Vec::new(),
            offered_items: Vec::new(),
            refuse_menu_open: None,
            alerts: Vec::new(),
            alert_focus: Vec::new(),
            alerts_withdrawn: Vec::new(),
            refuse_alert: None,
            hand_overs: Vec::new(),
            spent: Vec::new(),
            hand_over: Ok(HandOverOutcome::NotRunning),
            reveals: Vec::new(),
            reveal: Ok(()),
            desktop: Ok(sample_desktop()),
        }
    }
}

/// The desktop the host tests report: a 1024x768 screen at the reference
/// density, in the dark appearance — not the default an app starts in.
fn sample_desktop() -> DesktopInfo {
    match DesktopInfo::new(1024, 768, 100, Appearance::Dark) {
        Ok(info) => info,
        Err(_) => unreachable!("a 1024x768 screen at 100% is in range"),
    }
}

impl FrameHost for RecordingHost {
    fn window_presented(
        &mut self,
        window_id: u64,
        _surface: &DisplayMode,
        frame: &[u8],
        damage: &DamageList,
    ) -> Result<(), Errno> {
        if let Some(errno) = self.refuse_frames {
            return Err(errno);
        }
        self.presented.push((window_id, frame.to_vec(), *damage));
        Ok(())
    }

    fn frame_refused(&mut self, window_id: u64, errno: Errno) {
        self.frames_refused.push((window_id, errno));
    }
}

impl WindowHost for RecordingHost {
    fn window_opened(
        &mut self,
        owner: ProcId,
        window_id: u64,
        surface: &DisplayMode,
        title: &str,
        sizing: WindowSizing,
    ) -> Result<(), Errno> {
        if self.refuse_open {
            return Err(Errno::WouldBlock);
        }
        self.opened
            .push((owner, window_id, *surface, String::from(title), sizing));
        Ok(())
    }

    fn layer_opened(
        &mut self,
        owner: ProcId,
        window_id: u64,
        surface: &DisplayMode,
        x: i32,
        y: i32,
        depth: LayerDepth,
    ) -> Result<(), Errno> {
        if let Some(err) = self.refuse_layer {
            return Err(err);
        }
        self.layers.push((owner, window_id, *surface, x, y, depth));
        Ok(())
    }

    fn layer_placed(
        &mut self,
        window_id: u64,
        x: i32,
        y: i32,
        depth: LayerDepth,
    ) -> Result<(), Errno> {
        if let Some(err) = self.refuse_layer {
            return Err(err);
        }
        self.layer_places.push((window_id, x, y, depth));
        Ok(())
    }

    fn layer_terrain(&mut self, window_id: u64, out: &mut [TerrainPlate]) -> Result<usize, Errno> {
        let _ = window_id;
        if let Some(err) = self.refuse_terrain {
            return Err(err);
        }
        let written = self.terrain.len().min(out.len());
        out[..written].copy_from_slice(&self.terrain[..written]);
        Ok(written)
    }

    fn popup_opened(
        &mut self,
        window_id: u64,
        parent_window_id: u64,
        offset_x: i32,
        offset_y: i32,
        surface: &DisplayMode,
    ) -> Result<(), Errno> {
        if self.refuse_popup {
            return Err(Errno::WouldBlock);
        }
        self.popups
            .push((window_id, parent_window_id, offset_x, offset_y, *surface));
        Ok(())
    }

    fn tool_opened(&mut self, window_id: u64, spec: &ToolSpec) -> Result<(), Errno> {
        if self.refuse_popup {
            return Err(Errno::WouldBlock);
        }
        self.tools.push((
            window_id,
            spec.transient.parent_window_id,
            spec.carry,
            String::from(spec.title.as_str()),
            spec.sizing,
        ));
        Ok(())
    }

    fn dialog_opened(
        &mut self,
        window_id: u64,
        focus: bool,
        spec: &DialogSpec,
    ) -> Result<(), Errno> {
        if self.refuse_popup {
            return Err(Errno::WouldBlock);
        }
        self.dialogs.push((
            window_id,
            spec.transient.parent_window_id,
            focus,
            spec.modality,
            String::from(spec.title.as_str()),
        ));
        Ok(())
    }

    fn window_resized(&mut self, window_id: u64, surface: &DisplayMode) -> Result<(), Errno> {
        if let Some(err) = self.refuse_resize {
            return Err(err);
        }
        self.resized.push((window_id, *surface));
        Ok(())
    }

    fn window_sizing_changed(&mut self, window_id: u64, sizing: WindowSizing) -> Result<(), Errno> {
        if let Some(err) = self.refuse_sizing {
            return Err(err);
        }
        self.resized_range.push((window_id, sizing));
        Ok(())
    }

    fn window_size_state_changed(
        &mut self,
        window_id: u64,
        state: WindowSizeState,
    ) -> Result<(), Errno> {
        if let Some(err) = self.refuse_size_state {
            return Err(err);
        }
        self.size_states.push((window_id, state));
        Ok(())
    }

    fn window_retitled(&mut self, window_id: u64, title: &str) -> Result<(), Errno> {
        if let Some(err) = self.refuse_retitle {
            return Err(err);
        }
        self.retitled.push((window_id, String::from(title)));
        Ok(())
    }

    fn window_closed(&mut self, window_id: u64) {
        self.closed.push(window_id);
    }

    fn pick_requested(
        &mut self,
        window_id: u64,
        purpose: &PickPurpose,
        focus: bool,
    ) -> Result<(), Errno> {
        if let Some(err) = self.refuse_pick {
            return Err(err);
        }
        self.picks.push((window_id, *purpose));
        self.pick_focus.push(focus);
        Ok(())
    }

    fn pick_refused(&mut self, caller: ProcId, window_id: u64, _reason: &'static str) {
        self.picks_refused.push((caller, window_id));
    }

    fn drag_requested(&mut self, window_id: u64, items: &DragItems) -> Result<(), Errno> {
        if let Some(err) = self.refuse_drag {
            return Err(err);
        }
        self.drags
            .push((window_id, String::from(items.first().as_str())));
        Ok(())
    }

    fn drag_verdict(
        &mut self,
        window_id: u64,
        serial: u32,
        verdict: Option<DropOperation>,
    ) -> Result<(), Errno> {
        self.verdicts.push((window_id, serial, verdict));
        Ok(())
    }

    fn drop_landing(&mut self, window_id: u64, serial: u32, step: &LandingStep) {
        self.landings.push((window_id, serial, *step));
    }

    fn save_offer_answered(
        &mut self,
        window_id: u64,
        serial: u32,
        accepts: bool,
    ) -> Result<(), Errno> {
        self.save_answers.push((window_id, serial, accepts));
        Ok(())
    }

    fn items_offer_answered(
        &mut self,
        window_id: u64,
        serial: u32,
        verdict: Option<DropOperation>,
    ) -> Result<(), Errno> {
        self.items_answers.push((window_id, serial, verdict));
        Ok(())
    }

    fn offered_count(&self, window_id: u64, drag: u32) -> Result<u32, Errno> {
        let count = self
            .offered_items
            .iter()
            .filter(|(window, offered, ..)| (*window, *offered) == (window_id, drag))
            .count();
        match u32::try_from(count) {
            Ok(0) | Err(_) => Err(Errno::NotFound),
            Ok(count) => Ok(count),
        }
    }

    fn offered_item(
        &self,
        window_id: u64,
        drag: u32,
        index: u32,
    ) -> Result<tairix_abi::window_ipc::OfferedItem<'_>, Errno> {
        self.offered_items
            .iter()
            .filter(|(window, offered, ..)| (*window, *offered) == (window_id, drag))
            .nth(usize::try_from(index).map_err(|_| Errno::NotFound)?)
            .map(|(_, _, path, id)| tairix_abi::window_ipc::OfferedItem { path, id: *id })
            .ok_or(Errno::NotFound)
    }

    fn save_into(
        &mut self,
        caller: ProcId,
        window_id: u64,
        serial: u32,
        location: u64,
    ) -> Result<(), Errno> {
        self.save_folders
            .push((caller, window_id, serial, location));
        Ok(())
    }

    fn hand_over_requested(
        &mut self,
        desk: &mut dyn HandOverDesk,
        caller: ProcId,
        run_path: &str,
        target: &HandOverTarget,
    ) -> Result<HandOverOutcome, Errno> {
        self.spent.push(desk.spend_activation(caller, self.focused));
        self.hand_overs
            .push((caller, String::from(run_path), *target));
        // A host that says it reached an instance really queues something,
        // so the engine's own half of the hand-over is exercised too.
        if self.hand_over == Ok(HandOverOutcome::Reached) {
            let entry = match target {
                HandOverTarget::Document(doc) => OpenEntry::Document {
                    name: doc.name,
                    grant: doc.grant.get(),
                    writable: doc.writable,
                },
                HandOverTarget::Pane(pane) => OpenEntry::Pane(String::from(pane.as_str())),
                HandOverTarget::Bare => OpenEntry::Path(String::from(run_path)),
            };
            if !desk.hand_over(caller, GRANTED, &mut || Ok(entry.clone())) {
                return Ok(HandOverOutcome::NotRunning);
            }
        }
        self.hand_over
    }

    fn reveal_requested(
        &mut self,
        _desk: &mut dyn HandOverDesk,
        caller: ProcId,
        window_id: u64,
        location: u64,
    ) -> Result<(), Errno> {
        self.reveals.push((caller, window_id, location));
        self.reveal
    }

    fn menu_open_requested(
        &mut self,
        window_id: u64,
        open_id: u64,
        anchor: WindowRegion,
        menu: &AppMenu,
    ) -> Result<(), Errno> {
        if let Some(err) = self.refuse_menu_open {
            return Err(err);
        }
        self.menu_opens.push((window_id, open_id, anchor, *menu));
        Ok(())
    }

    fn alert_opened(&mut self, open: &AlertOpen<'_>) -> Result<(), Errno> {
        if let Some(err) = self.refuse_alert {
            return Err(err);
        }
        self.alerts
            .push((open.alert_id, open.owner, open.window, *open.spec));
        self.alert_focus.push(open.focus);
        Ok(())
    }

    fn alert_withdrawn(&mut self, alert_id: u64) {
        self.alerts_withdrawn.push(alert_id);
    }

    fn tooltip_declared(
        &mut self,
        window_id: u64,
        region: WindowRegion,
        text: &str,
    ) -> Result<(), Errno> {
        if let Some(err) = self.refuse_tooltip {
            return Err(err);
        }
        self.tooltips.push((window_id, region, String::from(text)));
        Ok(())
    }

    fn cursor_set(&mut self, window_id: u64, shape: CursorShape) -> Result<(), Errno> {
        self.cursors.push((window_id, shape));
        Ok(())
    }

    fn focused_window(&self) -> Option<u64> {
        self.focused
    }

    fn raise_requested(&mut self, window_id: u64) -> Result<(), Errno> {
        self.raised.push(window_id);
        Ok(())
    }

    fn raise_refused(&mut self, caller: ProcId, window_id: u64) {
        self.raises_refused.push((caller, window_id));
    }

    fn app_bar_declared(&mut self, owner: ProcId, bar: &AppBar) -> Result<(), Errno> {
        if let Some(err) = self.refuse_app_bar {
            return Err(err);
        }
        self.app_bars.push((owner, *bar));
        Ok(())
    }

    fn app_bar_withdrawn(&mut self, owner: ProcId) {
        self.app_bars_withdrawn.push(owner);
    }

    fn backdrop_blur_set(&mut self, window: u64, radius_px: u16) {
        self.blur_sets.push((window, radius_px));
    }

    fn desktop(&mut self) -> Result<DesktopInfo, Errno> {
        self.desktop
    }

    fn wallpaper_catalog(&mut self) -> &[WallpaperName] {
        &self.wallpapers
    }

    fn cursor_sets(&mut self) -> &[CursorSetName] {
        &self.cursor_sets
    }

    fn notify_sources(&mut self, caller: Option<&AppIdentity>) -> Result<&[BundleId], Errno> {
        self.asked_by.push(caller.copied());
        if caller.is_none() {
            return Err(Errno::PermissionDenied);
        }
        Ok(&self.notify_sources)
    }

    fn lock_screen(&mut self, caller: Option<&AppIdentity>) -> Result<(), Errno> {
        self.asked_by.push(caller.copied());
        if caller.is_none() {
            return Err(Errno::PermissionDenied);
        }
        self.locks += 1;
        Ok(())
    }

    fn zone_store(&mut self, to: ProcId, caller: Option<&AppIdentity>) -> Result<u64, Errno> {
        self.asked_by.push(caller.copied());
        if caller.is_none() {
            return Err(Errno::PermissionDenied);
        }
        self.zone_grants.push(to);
        Ok(ZONE_GRANT)
    }

    fn authorise_requested(
        &mut self,
        caller: Option<&AppIdentity>,
        asker: AuthoriseFor,
        task: &SystemTask,
    ) -> Result<(), Errno> {
        self.authorisations.push((caller.copied(), asker, *task));
        if caller.is_none() {
            return Err(Errno::PermissionDenied);
        }
        self.refuse_authorise.map_or(Ok(()), Err)
    }

    fn preview_render_requested(
        &mut self,
        caller: Option<&AppIdentity>,
        window_id: u64,
        region: ClientRegion,
        request: PreviewSize,
    ) -> Result<(), Errno> {
        if let Some(err) = self.refuse_render {
            return Err(err);
        }
        self.render_callers.push(caller.copied());
        self.renders.push((window_id, region, request));
        Ok(())
    }

    fn screensaver_preview_requested(
        &mut self,
        caller: Option<&AppIdentity>,
        document: &str,
    ) -> Result<(), Errno> {
        self.asked_by.push(caller.copied());
        if caller.is_none() {
            return Err(Errno::PermissionDenied);
        }
        self.previews.push(String::from(document));
        Ok(())
    }

    fn clipboard_set(
        &mut self,
        window_id: u64,
        region: ClientRegion,
        _len: u64,
        _kind: ClipboardKind,
    ) -> Result<(), Errno> {
        self.clipboard.push((window_id, region));
        Ok(())
    }

    fn clipboard_get(
        &mut self,
        window_id: u64,
        region: ClientRegion,
    ) -> Result<ClipboardHeld, Errno> {
        self.clipboard.push((window_id, region));
        Ok(ClipboardHeld {
            kind: None,
            len: 0,
            copied: false,
        })
    }
}

/// A host implementing only the mandatory bridge methods, so the trait's
/// own defaults run untouched.
struct MinimalHost;

impl WindowHost for MinimalHost {
    fn window_opened(
        &mut self,
        _owner: ProcId,
        _window_id: u64,
        _surface: &DisplayMode,
        _title: &str,
        _sizing: WindowSizing,
    ) -> Result<(), Errno> {
        Ok(())
    }

    fn window_resized(&mut self, _window_id: u64, _surface: &DisplayMode) -> Result<(), Errno> {
        Ok(())
    }

    fn window_sizing_changed(
        &mut self,
        _window_id: u64,
        _sizing: WindowSizing,
    ) -> Result<(), Errno> {
        Ok(())
    }

    fn window_size_state_changed(
        &mut self,
        _window_id: u64,
        _state: WindowSizeState,
    ) -> Result<(), Errno> {
        Ok(())
    }

    fn window_retitled(&mut self, _window_id: u64, _title: &str) -> Result<(), Errno> {
        Ok(())
    }

    fn window_closed(&mut self, _window_id: u64) {}

    fn pick_requested(
        &mut self,
        _window_id: u64,
        _purpose: &PickPurpose,
        _focus: bool,
    ) -> Result<(), Errno> {
        Ok(())
    }

    fn desktop(&mut self) -> Result<DesktopInfo, Errno> {
        Ok(sample_desktop())
    }
}

/// A sink recording each delivered event by endpoint, doubling as the
/// backing queue the client-side event source pops.
#[derive(Default)]
struct QueueSink {
    delivered: VecDeque<(u64, [u8; WindowEvent::WIRE_LEN])>,
    /// The `(endpoint, window)` a render conclusion is held undelivered for.
    held_render: Option<(u64, u64)>,
}

impl EventSink for QueueSink {
    fn deliver(&mut self, endpoint: u64, event: &WindowEvent) -> Result<(), Errno> {
        self.delivered.push_back((endpoint, event.to_le_bytes()));
        Ok(())
    }

    fn holds_render(&self, endpoint: u64, window_id: u64) -> bool {
        self.held_render == Some((endpoint, window_id))
    }
}

/// A sink that refuses everything with the owner's mailbox-full signal — the
/// back-pressure a session takes responsibility for.
struct FullSink;

impl EventSink for FullSink {
    fn deliver(&mut self, _endpoint: u64, _event: &WindowEvent) -> Result<(), Errno> {
        Err(Errno::WouldBlock)
    }
}

/// The in-process loopback: a real server behind the client seams.
struct Loopback {
    server: WindowServer<MockMapper>,
    host: RecordingHost,
    identity: MockIdentity,
    /// Where an event the engine sends while serving a request lands — the
    /// hand-over's wake is the one that does.
    sink: QueueSink,
    /// The fixed ticket naming who the "kernel" attests the next call as
    /// coming from.
    ticket: u64,
    /// The ticket the next call is served under: minted per call, as the
    /// kernel's are, so a held answer can be told from the next.
    next_call: u64,
    /// Every frame the client put on the wire, in order.
    sent: alloc::vec::Vec<alloc::vec::Vec<u8>>,
    /// Whether a posted request waits to be served until
    /// [`Loopback::serve_posted`], as one does while the session is busy.
    holds_posts: bool,
    /// Posted requests not yet served, oldest first, by post ticket.
    unserved: VecDeque<(u64, alloc::vec::Vec<u8>)>,
    /// Replies to served posts not yet reaped, by post ticket.
    answered: alloc::vec::Vec<(u64, alloc::vec::Vec<u8>)>,
    /// The ticket the next post is given.
    next_post: u64,
    /// The depth this client's place at the endpoint holds, counting what is
    /// posted, what the session holds unanswered, and what is answered and not
    /// yet reaped.
    lane: Option<usize>,
    /// Whether presented frames wait to be shown until [`Loopback::latch`],
    /// as they do between the desktop's frames, rather than being shown as
    /// each request is served.
    holds_frames: bool,
    /// Every post ticket a client waited on, in order.
    awaited: alloc::vec::Vec<u64>,
    /// How many times a client asked after a posted request's reply.
    reaps: usize,
    /// Posts whose answer the engine withheld: the ticket each was served
    /// under, and its post ticket.
    withheld: alloc::vec::Vec<(u64, u64)>,
    /// Answers the engine sent that no post was waiting on, by the ticket the
    /// call was served under.
    called: alloc::vec::Vec<(u64, alloc::vec::Vec<u8>)>,
    /// How many desktop frames have been latched.
    latches: usize,
    /// Why a wait for a reply ends at once with the call left as it was, as
    /// an await the kernel interrupts does.
    refuses_awaits: Option<Errno>,
}

impl Loopback {
    fn with_regions(regions: &[(u64, usize)]) -> Rc<RefCell<Self>> {
        Self::with_mapper(MockMapper::with_regions(regions))
    }

    fn with_mapper(mapper: MockMapper) -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self {
            server: WindowServer::new(mapper, SERVER, CLIENT_FRAME_MAX),
            host: RecordingHost::default(),
            identity: MockIdentity::holding_layer(&[TICKET_A, TICKET_B]),
            sink: QueueSink::default(),
            ticket: TICKET_A,
            next_call: MINTED_TICKETS,
            sent: alloc::vec::Vec::new(),
            holds_posts: false,
            unserved: VecDeque::new(),
            answered: alloc::vec::Vec::new(),
            next_post: 1,
            lane: None,
            holds_frames: false,
            awaited: alloc::vec::Vec::new(),
            reaps: 0,
            withheld: alloc::vec::Vec::new(),
            called: alloc::vec::Vec::new(),
            latches: 0,
            refuses_awaits: None,
        }))
    }

    /// Show every frame presented since the last, as the desktop's next frame
    /// does, and send the answers that released.
    fn latch(&mut self) {
        self.latches += 1;
        self.server.latch(&mut self.host);
        self.collect_answers();
    }

    /// Serve one request frame under a ticket minted for it, as the kernel
    /// mints one per call: the ticket, and the reply unless the engine
    /// withheld it.
    fn serve_request(&mut self, request: &[u8]) -> (u64, Option<alloc::vec::Vec<u8>>) {
        let ticket = self.next_call;
        self.next_call += 1;
        self.identity.minted = Some((ticket, self.ticket));
        let mut frame = [0u8; WINDOW_REPLY_MAX];
        let served = self.server.serve(
            &mut self.host,
            &mut self.sink,
            &mut self.identity,
            ticket,
            request,
            &mut frame,
        );
        self.identity.minted = None;
        let reply = match served {
            Served::Reply(len) => Some(frame[..len].to_vec()),
            Served::Withheld => None,
        };
        (ticket, reply)
    }

    /// What the session does once a request is served: show what it presented
    /// unless frames wait for the desktop's next, and send every answer due.
    fn after_serve(&mut self) {
        if self.holds_frames {
            self.collect_answers();
        } else {
            self.latch();
        }
    }

    /// Take every answer the engine has due, each to the post it answers or,
    /// for a call, kept by the ticket it was served under.
    fn collect_answers(&mut self) {
        let mut sent = alloc::vec::Vec::new();
        self.server.send_answers(|ticket, reply| {
            sent.push((ticket, reply.to_vec()));
        });
        for (ticket, reply) in sent {
            match self
                .withheld
                .iter()
                .position(|&(served, _)| served == ticket)
            {
                Some(index) => {
                    let (_, post) = self.withheld.swap_remove(index);
                    self.answered.push((post, reply));
                }
                None => self.called.push((ticket, reply)),
            }
        }
    }

    /// The answer to the call served as `ticket`, which the engine withheld:
    /// the desktop's next frame releases it, so one latch runs, and an answer
    /// even that leaves held is a stall.
    fn held_answer(&mut self, ticket: u64) -> alloc::vec::Vec<u8> {
        if !self.called.iter().any(|(at, _)| *at == ticket) {
            self.latch();
        }
        let Some(index) = self.called.iter().position(|(at, _)| *at == ticket) else {
            panic!("a call stalled: the desktop's frame did not answer it");
        };
        self.called.swap_remove(index).1
    }

    /// Serve every posted request still waiting, oldest first, as the session
    /// reaches them.
    fn serve_posted(&mut self) {
        while self.serve_next().is_some() {}
    }

    /// Serve the posted requests waiting, oldest first, up to and including
    /// `ticket`: the session reaches a client's requests in order.
    fn serve_through(&mut self, ticket: u64) {
        while self.serve_next().is_some_and(|served| served != ticket) {}
    }

    /// Serve the oldest posted request waiting, answering its post ticket.
    fn serve_next(&mut self) -> Option<u64> {
        let (post, request) = self.unserved.pop_front()?;
        let (ticket, reply) = self.serve_request(&request);
        match reply {
            Some(reply) => self.answered.push((post, reply)),
            None => self.withheld.push((ticket, post)),
        }
        self.after_serve();
        Some(post)
    }
}

/// Deliver one app-ward event through the real server, bridged to the
/// loopback's own host — the pair a session holds together, so a teardown
/// the delivery triggers reaches the compositor as it would in production.
fn deliver(
    loopback: &Rc<RefCell<Loopback>>,
    sink: &mut dyn EventSink,
    event: &WindowEvent,
) -> Result<(), Errno> {
    let inner = &mut *loopback.borrow_mut();
    inner.server.deliver_event(sink, event)
}

/// Conclude `window`'s pick with `picked` — a handle, a name, and whether it
/// was delegated writable — or with nothing chosen.
fn conclude(
    loopback: &Rc<RefCell<Loopback>>,
    sink: &mut dyn EventSink,
    window: u64,
    picked: Option<(u64, &str, bool)>,
) -> Result<(), Errno> {
    let picked = picked.map(|(handle, name, writable)| {
        (
            handle,
            DocumentName::new(name).expect("a valid name"),
            writable,
        )
    });
    loopback.borrow_mut().server.conclude_pick(
        sink,
        window,
        picked.as_ref().map(|(handle, name, writable)| PickedFile {
            handle: *handle,
            name,
            writable: *writable,
        }),
    )
}

/// A window's picker, asked to choose a file to open.
const OPEN: PickPurpose = PickPurpose::Open;

/// One request encoded exactly as a client sends it: a frame of its own
/// operation's length, which is what the server is given on the wire.
fn request_frame(request: &WindowRequest) -> alloc::vec::Vec<u8> {
    let mut out = alloc::vec![0u8; WindowRequest::MAX_WIRE_LEN];
    let len = request.encode(&mut out).expect("the max frame fits");
    out.truncate(len);
    out
}

/// The length of the reply `served` answered at once.
fn replied(served: Served) -> usize {
    match served {
        Served::Reply(len) => len,
        Served::Withheld => panic!("only a present replacing an unshown frame is withheld"),
    }
}

/// A present of frame `frame_index` of `window_id` declaring the whole
/// surface, as the wire carries it.
fn present_frame(window_id: u64, frame_index: u32) -> alloc::vec::Vec<u8> {
    request_frame(&WindowRequest::Present {
        window_id,
        frame_index,
        damage: full_damage(),
    })
}

impl WindowTransport for Rc<RefCell<Loopback>> {
    fn call(&mut self, request: &[u8], reply: &mut [u8]) -> Result<usize, Errno> {
        let inner = &mut *self.borrow_mut();
        inner.sent.push(request.to_vec());
        // The endpoint serves a client's requests in order, so whatever it
        // posted earlier is served before this call.
        inner.serve_posted();
        let (ticket, served) = inner.serve_request(request);
        inner.after_serve();
        let frame = served.unwrap_or_else(|| inner.held_answer(ticket));
        let out = reply.get_mut(..frame.len()).ok_or(Errno::BufferTooSmall)?;
        out.copy_from_slice(&frame);
        Ok(frame.len())
    }

    fn post(&mut self, request: &[u8]) -> Result<u64, Errno> {
        let inner = &mut *self.borrow_mut();
        let held = inner.unserved.len() + inner.answered.len() + inner.withheld.len();
        if inner.lane.is_some_and(|depth| held >= depth) {
            return Err(Errno::WouldBlock);
        }
        inner.sent.push(request.to_vec());
        let ticket = inner.next_post;
        inner.next_post += 1;
        inner.unserved.push_back((ticket, request.to_vec()));
        if !inner.holds_posts {
            inner.serve_posted();
        }
        Ok(ticket)
    }

    fn reap(&mut self, ticket: u64, reply: &mut [u8]) -> Result<Option<usize>, Errno> {
        let inner = &mut *self.borrow_mut();
        inner.reaps += 1;
        let Some(index) = inner.answered.iter().position(|(at, _)| *at == ticket) else {
            return if inner.unserved.iter().any(|(at, _)| *at == ticket)
                || inner.withheld.iter().any(|&(_, post)| post == ticket)
            {
                Ok(None)
            } else {
                Err(Errno::NotFound)
            };
        };
        let (_, frame) = inner.answered.swap_remove(index);
        let out = reply.get_mut(..frame.len()).ok_or(Errno::BufferTooSmall)?;
        out.copy_from_slice(&frame);
        Ok(Some(frame.len()))
    }

    fn await_reply(&mut self, ticket: u64, reply: &mut [u8]) -> Result<usize, Errno> {
        {
            let inner = &mut *self.borrow_mut();
            inner.awaited.push(ticket);
            if let Some(errno) = inner.refuses_awaits {
                return Err(errno);
            }
            // A waited-on post is reached however busy the session is, after
            // those posted before it and before any posted since.
            inner.serve_through(ticket);
            // A held answer comes with the desktop's next frame; one that even
            // that leaves held is a stall.
            if inner.withheld.iter().any(|&(_, post)| post == ticket) {
                inner.latch();
                assert!(
                    inner.withheld.iter().all(|&(_, post)| post != ticket),
                    "a present stalled: the desktop's frame did not answer it"
                );
            }
        }
        self.reap(ticket, reply)?.ok_or(Errno::NotFound)
    }
}

/// The client-side event source popping the recorded deliveries for one
/// endpoint (the app's own event endpoint in production).
///
/// Nothing here can be woken, so the park stands in for one that never
/// returns: it refuses, which is what lets a test assert that the drain was
/// tried first and that a park happened at all.
struct QueueSource {
    queue: VecDeque<[u8; WindowEvent::WIRE_LEN]>,
    /// How many times the source has been parked, shared so a test can read
    /// it once a waiter owns the source.
    parked: Rc<Cell<usize>>,
}

impl QueueSource {
    /// A source over `frames`, in delivery order.
    fn new(frames: impl IntoIterator<Item = [u8; WindowEvent::WIRE_LEN]>) -> Self {
        Self {
            queue: frames.into_iter().collect(),
            parked: Rc::default(),
        }
    }
}

impl EventDrain for QueueSource {
    fn try_next(&mut self, event: &mut [u8; WindowEvent::WIRE_LEN]) -> Result<bool, Errno> {
        let Some(frame) = self.queue.pop_front() else {
            return Ok(false);
        };
        *event = frame;
        Ok(true)
    }
}

impl EventSource for QueueSource {
    fn park(&mut self) -> Result<Parked, Errno> {
        self.parked.set(self.parked.get() + 1);
        Err(Errno::WouldBlock)
    }
}

/// A source whose park is interrupted by something the loop owns — a worker's
/// answer — rather than by an event.
#[derive(Default)]
struct InterruptingSource {
    /// How many times the source has been parked.
    parked: Rc<Cell<usize>>,
}

impl EventDrain for InterruptingSource {
    fn try_next(&mut self, _event: &mut [u8; WindowEvent::WIRE_LEN]) -> Result<bool, Errno> {
        Ok(false)
    }
}

impl EventSource for InterruptingSource {
    fn park(&mut self) -> Result<Parked, Errno> {
        self.parked.set(self.parked.get() + 1);
        Ok(Parked::Interrupted)
    }
}

/// The regression the answer type exists for. A worker's wake is level-
/// triggered, so a wait that treated it as "no event yet" would park again on
/// a source that is *still* ready — a spin, not a wait, and the answer would
/// never reach the loop. One interrupted park ends the wait with no event,
/// and it is the only park: the wait returns to the loop rather than going
/// round again.
#[test]
fn an_interrupted_park_ends_the_wait_without_an_event() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let source = InterruptingSource::default();
    let parked = Rc::clone(&source.parked);
    let mut waiter = WindowEvents::new(source);
    assert_eq!(waiter.wait(&mut client), Ok(None));
    assert_eq!(parked.get(), 1);
}

/// More parks than any wait in these tests takes; one past it is a spin.
const SPIN_BOUND: usize = 16;

/// An app's wait-set as the kernel keeps it: the reply member is ready while
/// an answer to a posted request lies untaken, and the first park is where
/// the desktop shows the frame that releases a held answer. With nothing
/// ready the park ends at the loop's own deadline.
struct ReplyWakeSource {
    loopback: Rc<RefCell<Loopback>>,
    parked: Rc<Cell<usize>>,
}

impl EventDrain for ReplyWakeSource {
    fn try_next(&mut self, _event: &mut [u8; WindowEvent::WIRE_LEN]) -> Result<bool, Errno> {
        Ok(false)
    }
}

impl EventSource for ReplyWakeSource {
    fn park(&mut self) -> Result<Parked, Errno> {
        let parks = self.parked.get() + 1;
        self.parked.set(parks);
        if parks > SPIN_BOUND {
            return Err(Errno::Busy);
        }
        let mut loopback = self.loopback.borrow_mut();
        if parks == 1 {
            loopback.latch();
        }
        // A reply wakes the park as an event does: it is the stream's to take.
        if loopback.answered.is_empty() {
            Ok(Parked::Interrupted)
        } else {
            Ok(Parked::Served)
        }
    }
}

/// An answer landing while the app is parked wakes it, and the wait takes the
/// answer in before parking again. Parked again with the answer untaken, the
/// reply member stays ready and every park returns at once, ahead of any
/// deadline the loop gave it: the spin that left Settings unable to adopt its
/// resized extent, and Patience's deal frozen until the pointer moved.
#[test]
fn an_answer_landing_during_the_park_is_taken_in_before_the_next() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    loopback.borrow_mut().holds_frames = true;
    hold_a_present(&mut client, window);

    let parked = Rc::default();
    let mut waiter = WindowEvents::new(ReplyWakeSource {
        loopback: Rc::clone(&loopback),
        parked: Rc::clone(&parked),
    });
    assert_eq!(waiter.wait(&mut client), Ok(None), "the deadline ends it");
    assert_eq!(parked.get(), 2, "woken by the answer, then by the deadline");
    assert!(!client.present_in_flight(window), "the answer was taken in");
    assert!(loopback.borrow().answered.is_empty());
}

/// Full damage over SURFACE.
fn full_damage() -> DamageList {
    one(DamageRect::full(&SURFACE))
}

/// A present naming `rect` alone.
fn one(rect: DamageRect) -> DamageList {
    DamageList::new(&[rect]).expect("a non-empty rectangle")
}

/// Create through `client`, returning just the minted window id (the
/// server stamp is asserted once in the loopback round trip).
fn create_id(
    client: &mut WindowClient<Rc<RefCell<Loopback>>>,
    shm: u64,
    events: u64,
    frames: u32,
    title: &str,
) -> Result<u64, Errno> {
    client
        .create(
            shm,
            events,
            frames,
            &SURFACE,
            title,
            WindowSizing::default(),
        )
        .map(|(id, _)| id)
}

/// A one-frame SURFACE-shaped popup of `parent`, granted as `shm`, its
/// events routed to `events`, offset `(offset_x, offset_y)` from the
/// parent's client origin.
fn popup_spec(
    parent_window_id: u64,
    shm: u64,
    events: u64,
    offset_x: i32,
    offset_y: i32,
) -> PopupSpec {
    PopupSpec {
        transient: transient_spec(parent_window_id, shm, events),
        offset_x,
        offset_y,
    }
}

/// A one-frame SURFACE-shaped transient of `parent`, granted as `shm`, its
/// events routed to `events`.
fn transient_spec(parent_window_id: u64, shm: u64, events: u64) -> TransientSpec {
    TransientSpec {
        parent_window_id,
        shm_handle: shm,
        event_endpoint: events,
        frame_count: 1,
        surface: SURFACE,
    }
}

/// A dialog of `owner` granted as `shm`, holding its owner as `modality`
/// says.
fn dialog_spec(owner: u64, shm: u64, modality: DialogModality) -> DialogSpec {
    DialogSpec {
        transient: transient_spec(owner, shm, EVENTS_A),
        modality,
        title: tairix_abi::window_ipc::WindowTitle::new("Go to line").expect("a title"),
    }
}

#[test]
fn an_app_learns_its_desktop_before_it_owns_a_window() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));

    // Nothing has been created: the query names no window precisely so an
    // app can size its first one to a screen it knows.
    assert_eq!(loopback.borrow().server.window_count(), 0);
    let info = client.desktop().expect("the session describes its desktop");
    assert_eq!(info, sample_desktop());

    let desktop = Desktop::new(info).expect("the reported scale is one Scale admits");
    assert_eq!(desktop.screen(), Rect::new(0, 0, 1024, 768));
    assert_eq!(desktop.scale(), Scale::ONE);
    assert_eq!(desktop.appearance(), Appearance::Dark);
    // At the reference density a logical size is its own physical size,
    // so the app's own choice stands — unless it exceeds the screen, which
    // caps it.
    assert_eq!(desktop.window_size(640, 480), (640, 480));
    assert_eq!(desktop.window_size(2000, 900), (1024, 768));
}

#[test]
fn a_window_is_sized_at_the_desktops_density_and_capped_to_the_screen() {
    let dense = DesktopInfo::new(1024, 768, 200, Appearance::Dark)
        .expect("a 1024x768 screen at 200% is in range");
    let desktop = Desktop::new(dense).expect("200% is one Scale admits");

    // The app authors its preference in logical pixels, so at twice the
    // density it asks for twice the pixels...
    assert_eq!(desktop.window_size(320, 240), (640, 480));
    // ...but never more than the screen it must appear on, which is what
    // a fixed-size window authored for a roomier desktop would otherwise
    // ask for once doubled.
    assert_eq!(desktop.window_size(800, 600), (1024, 768));
}

#[test]
fn a_session_with_no_desktop_refuses_rather_than_inventing_one() {
    let loopback = Loopback::with_regions(&[]);
    loopback.borrow_mut().host.desktop = Err(Errno::NotFound);
    let mut client = WindowClient::new(Rc::clone(&loopback));

    assert_eq!(client.desktop(), Err(Errno::NotFound));
}

/// The desktop belongs to the seat, not to one window, so an application
/// converges on the published state rather than being told per window: it
/// adopts a new record once and answers `false` for a re-publish of the same
/// one, so a session that re-states the current desktop costs no repaint.
#[test]
fn adopting_a_published_desktop_is_news_only_once() {
    let mut desktop = Desktop::new(sample_desktop()).expect("the sample scale is in range");
    let switched = DesktopInfo::new(1024, 768, 100, Appearance::Light)
        .expect("a 1024x768 screen at 100% is in range");

    assert_eq!(desktop.adopt(switched), Ok(true));
    assert_eq!(desktop.appearance(), Appearance::Light);
    assert_eq!(desktop.adopt(switched), Ok(false));
    assert_eq!(desktop.appearance(), Appearance::Light);

    // Back again is a change in its own right: an application must be told
    // it may return to the appearance it started in.
    assert_eq!(desktop.adopt(sample_desktop()), Ok(true));
    assert_eq!(desktop.appearance(), Appearance::Dark);
}

/// A publication of the desktop's settings document changes no pixel a window
/// draws: the generation is taken for an application keeping settings there to
/// read them again, and no repaint is owed for it.
#[test]
fn a_new_settings_generation_alone_is_taken_without_a_redraw() {
    let mut desktop = Desktop::new(sample_desktop()).expect("the sample scale is in range");
    assert_eq!(
        desktop.adopt(sample_desktop().with_settings_generation(3)),
        Ok(false)
    );
    assert_eq!(desktop.settings_generation(), 3);
    let relit = DesktopInfo::new(1024, 768, 100, Appearance::Light)
        .expect("a 1024x768 screen at 100% is in range")
        .with_settings_generation(4);
    assert_eq!(
        desktop.adopt(relit),
        Ok(true),
        "anything else still redraws"
    );
}

/// A display retimed at the same resolution changes no pixel the window
/// draws: the new rate is taken for pacing, and no repaint is owed for it.
#[test]
fn a_new_frame_rate_alone_is_taken_without_a_redraw() {
    let mut desktop = Desktop::new(sample_desktop()).expect("the sample scale is in range");
    let retimed = RefreshRate::from_mhz(75_000).expect("in bounds");

    assert_eq!(
        desktop.adopt(sample_desktop().with_refresh(retimed)),
        Ok(false)
    );
    assert_eq!(desktop.refresh(), retimed);
    assert_eq!(desktop.info(), sample_desktop().with_refresh(retimed));

    let relit = DesktopInfo::new(1024, 768, 100, Appearance::Light)
        .expect("a 1024x768 screen at 100% is in range")
        .with_refresh(RefreshRate::ASSUMED);
    assert_eq!(
        desktop.adopt(relit),
        Ok(true),
        "anything else still redraws"
    );
    assert_eq!(desktop.refresh(), RefreshRate::ASSUMED);
}

#[test]
fn a_desktop_the_client_cannot_draw_at_is_refused_and_the_last_good_one_stands() {
    let mut desktop = Desktop::new(sample_desktop()).expect("the sample scale is in range");

    // The wire admits any non-zero percentage; what a *usable* scale is
    // belongs to the geometry type, so a percentage outside its range is
    // refused rather than clamped to something the session did not ask
    // for.
    let absurd = DesktopInfo::new(1024, 768, 5, Appearance::Light)
        .expect("the wire accepts any non-zero percentage");
    assert_eq!(Desktop::new(absurd), Err(Errno::OutOfRange));
    assert_eq!(desktop.adopt(absurd), Err(Errno::OutOfRange));
    assert_eq!(desktop.scale(), Scale::ONE);
    assert_eq!(desktop.appearance(), Appearance::Dark);
}

#[test]
fn create_present_close_round_trips_through_the_loopback() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));

    let (window, server) = client
        .create(7, EVENTS_A, 2, &SURFACE, "Files", WindowSizing::default())
        .expect("a valid create succeeds");
    assert_eq!(window, 1);
    assert_eq!(
        server, SERVER,
        "the reply is stamped with the session identity apps authenticate events against"
    );
    {
        let inner = loopback.borrow();
        assert_eq!(inner.server.window_count(), 1);
        assert_eq!(
            inner.host.opened,
            alloc::vec![(
                proc_id(0xA1),
                1,
                SURFACE,
                String::from("Files"),
                WindowSizing::default()
            )],
            "the host is told the kernel-attested owner, not anything the client said"
        );
    }

    // Present the second frame: the host sees exactly that frame's bytes.
    let damage = DamageRect {
        x: 1,
        y: 1,
        width_px: 2,
        height_px: 2,
    };
    client
        .present(window, 1, one(damage))
        .expect("present succeeds");
    {
        let inner = loopback.borrow();
        let (id, frame, seen) = &inner.host.presented[0];
        assert_eq!(*id, window);
        assert_eq!(*seen, one(damage));
        let expected: Vec<u8> = (FRAME_LEN..2 * FRAME_LEN)
            .map(|i| region_byte(7, i))
            .collect();
        assert_eq!(*frame, expected);
    }

    client.close(window).expect("close succeeds");
    {
        let inner = loopback.borrow();
        assert_eq!(inner.server.window_count(), 0);
        assert_eq!(inner.host.closed, alloc::vec![window]);
    }
    // The window is gone: a second close finds nothing.
    assert_eq!(client.close(window), Err(Errno::NotFound));
}

#[test]
fn create_forwards_the_sizing_contract_to_the_host() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let floor = WindowSizing::Resizable {
        min_width_px: 240,
        min_height_px: 160,
        max_width_px: 0,
        max_height_px: 0,
    };

    client
        .create(7, EVENTS_A, 1, &SURFACE, "fixed", WindowSizing::Fixed)
        .expect("fixed window");
    client
        .create(8, EVENTS_A, 1, &SURFACE, "resizable", floor)
        .expect("resizable window");

    let inner = loopback.borrow();
    let sizings: Vec<WindowSizing> = inner
        .host
        .opened
        .iter()
        .map(|(_, _, _, _, sizing)| *sizing)
        .collect();
    // The host is the enforcer, so it receives the app's floor verbatim:
    // nothing between the two rounds, clamps, or drops it.
    assert_eq!(sizings, alloc::vec![WindowSizing::Fixed, floor]);
}

#[test]
fn a_below_minimum_resize_is_not_answered_with_a_resize_of_the_client_s_own() {
    // The size is negotiated once, at create: the app declares its floor
    // and the window manager enforces it. A reported size below that floor
    // therefore leaves the wire silent — an app that answered it by
    // resizing itself back up would fight the drag, frame by frame, which
    // is the flicker this negotiation removes.
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let floor = WindowSizing::Resizable {
        min_width_px: 240,
        min_height_px: 160,
        max_width_px: 0,
        max_height_px: 0,
    };
    let (window, _) = client
        .create(7, EVENTS_A, 1, &SURFACE, "Files", floor)
        .expect("create");

    let under = WindowEvent::Resized {
        window_id: window,
        width_px: 1,
        height_px: 1,
        state: WindowSizeState::Restored,
    };
    let mut waiter = WindowEvents::new(QueueSource::new([under.to_le_bytes()]));
    assert_eq!(waiter.wait(&mut client), Ok(Some(under)));

    let inner = loopback.borrow();
    assert!(
        inner.host.resized.is_empty(),
        "the reported size is laid out at, never bounced back as a resize"
    );
    assert_eq!(inner.server.window_count(), 1);
}

#[test]
fn resize_remaps_the_window_and_presents_at_the_new_size() {
    // A larger surface the window is resized onto.
    const BIG: DisplayMode = DisplayMode {
        width_px: 8,
        height_px: 3,
        stride_bytes: 32,
        format: DisplayFormat::Bgra8888,
    };
    const BIG_FRAME_LEN: usize = 96;

    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN), (20, 2 * BIG_FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));

    let window = create_id(&mut client, 7, EVENTS_A, 2, "Files").expect("create succeeds");

    // Re-map the window onto the larger region: the host is told the new
    // geometry, the id and count are unchanged.
    client.resize(window, 20, 2, &BIG).expect("resize succeeds");
    {
        let inner = loopback.borrow();
        assert_eq!(inner.server.window_count(), 1);
        assert_eq!(inner.host.resized, alloc::vec![(window, BIG)]);
    }

    // A present now shapes its frame against the new surface: full damage
    // of the *larger* surface is accepted (it would have been out of
    // bounds against the old one).
    let damage = DamageRect::full(&BIG);
    client
        .present(window, 1, one(damage))
        .expect("present at new size");
    {
        let inner = loopback.borrow();
        let (id, frame, seen) = inner.host.presented.last().expect("a present");
        assert_eq!(*id, window);
        assert_eq!(*seen, one(damage));
        assert_eq!(frame.len(), BIG_FRAME_LEN);
    }
}

#[test]
fn resize_is_refused_fail_closed() {
    const BIG: DisplayMode = DisplayMode {
        width_px: 8,
        height_px: 3,
        stride_bytes: 32,
        format: DisplayFormat::Bgra8888,
    };

    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN), (20, 2 * 96)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));

    let window = create_id(&mut client, 7, EVENTS_A, 2, "Files").expect("create succeeds");

    // A window the caller does not own answers like one that never
    // existed, and the host is never told; the size the session kept is
    // delivered again.
    loopback.borrow_mut().ticket = TICKET_B;
    client.resize(window, 20, 2, &BIG).expect("posted");
    assert_eq!(
        refusal(&mut client),
        Some(refused(window, PostedRequest::Resize, Errno::NotFound))
    );
    assert_eq!(client.take_owed(), Some(Ok(kept(window, &SURFACE))));
    assert!(loopback.borrow().host.resized.is_empty());
    loopback.borrow_mut().ticket = TICKET_A;

    // A host that refuses the resize leaves the old geometry intact: the
    // window still presents at its original size, not the (refused) new one.
    loopback.borrow_mut().host.refuse_resize = Some(Errno::WouldBlock);
    client.resize(window, 20, 2, &BIG).expect("posted");
    assert_eq!(
        refusal(&mut client),
        Some(refused(window, PostedRequest::Resize, Errno::WouldBlock))
    );
    assert_eq!(client.take_owed(), Some(Ok(kept(window, &SURFACE))));
    loopback.borrow_mut().host.refuse_resize = None;
    client
        .present(window, 0, full_damage())
        .expect("still presents at the original size");
    assert_eq!(refusal(&mut client), None);
    // Full damage of the larger surface is still out of bounds — the
    // window was never actually resized.
    client
        .present(window, 0, one(DamageRect::full(&BIG)))
        .expect("posted");
    assert_eq!(
        refusal(&mut client),
        Some(refused(
            window,
            PostedRequest::Present,
            Errno::LengthOutOfRange
        ))
    );
}

#[test]
fn window_ids_are_minted_monotonically_and_never_reused() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));

    let first = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("first");
    let second = create_id(&mut client, 7, EVENTS_A, 1, "b").expect("second");
    client.close(first).expect("close");
    let third = create_id(&mut client, 7, EVENTS_A, 1, "c").expect("third");
    assert_eq!((first, second, third), (1, 2, 3));
}

#[test]
fn a_caller_cannot_touch_another_clients_window() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));

    let window = create_id(&mut client, 7, EVENTS_A, 1, "A's").expect("A creates");

    // B presents and closes A's window: refused exactly like a window
    // that does not exist, and A's window survives untouched.
    loopback.borrow_mut().ticket = TICKET_B;
    client.present(window, 0, full_damage()).expect("posted");
    assert_eq!(
        refusal(&mut client),
        Some(refused(window, PostedRequest::Present, Errno::NotFound))
    );
    client.set_title(window, "B's").expect("posted");
    client
        .set_sizing(window, WindowSizing::Fixed)
        .expect("posted");
    assert_eq!(
        refusal(&mut client),
        Some(refused(window, PostedRequest::Title, Errno::NotFound))
    );
    assert_eq!(
        refusal(&mut client),
        Some(refused(window, PostedRequest::Sizing, Errno::NotFound))
    );
    assert_eq!(client.close(window), Err(Errno::NotFound));
    {
        let inner = loopback.borrow();
        assert_eq!(inner.server.window_count(), 1);
        assert!(inner.host.closed.is_empty());
        assert!(inner.host.retitled.is_empty());
        assert!(inner.host.resized_range.is_empty());
    }

    // A still owns it.
    loopback.borrow_mut().ticket = TICKET_A;
    client
        .present(window, 0, full_damage())
        .expect("A presents");
}

#[test]
fn an_owner_restates_its_window_s_range_and_a_refusal_changes_nothing() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));

    let window = create_id(&mut client, 7, EVENTS_A, 1, "Sapper").expect("A creates");
    let range = WindowSizing::Resizable {
        min_width_px: 300,
        min_height_px: 200,
        max_width_px: 900,
        max_height_px: 700,
    };
    client
        .set_sizing(window, range)
        .expect("the owner restates its range");
    assert_eq!(loopback.borrow().host.resized_range, [(window, range)]);

    // A range naming no reachable size never reaches the session.
    assert_eq!(
        client.set_sizing(
            window,
            WindowSizing::Resizable {
                min_width_px: 300,
                min_height_px: 200,
                max_width_px: 100,
                max_height_px: 700,
            },
        ),
        Err(Errno::OutOfRange)
    );
    // An unknown window is refused, and a host refusal leaves the
    // previous range standing.
    client.set_sizing(window + 1, range).expect("posted");
    assert_eq!(
        refusal(&mut client),
        Some(refused(window + 1, PostedRequest::Sizing, Errno::NotFound))
    );
    loopback.borrow_mut().host.refuse_sizing = Some(Errno::NotSupported);
    client.set_sizing(window, range).expect("posted");
    assert_eq!(
        refusal(&mut client),
        Some(refused(window, PostedRequest::Sizing, Errno::NotSupported))
    );
    assert_eq!(loopback.borrow().host.resized_range.len(), 1);
}

#[test]
fn an_owner_asks_for_a_size_state_and_a_refusal_changes_nothing() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));

    let window = create_id(&mut client, 7, EVENTS_A, 1, "WinterSun").expect("A creates");
    client
        .set_size_state(window, WindowSizeState::Fullscreen)
        .expect("the owner asks for fullscreen");
    assert_eq!(
        loopback.borrow().host.size_states,
        [(window, WindowSizeState::Fullscreen)]
    );

    // A window the caller does not own never reaches the session: the id
    // is a name, not a credential.
    assert_eq!(
        client.set_size_state(window + 1, WindowSizeState::Fullscreen),
        Err(Errno::NotFound)
    );
    assert_eq!(loopback.borrow().host.size_states.len(), 1);

    // A host refusal is relayed and leaves the window where it is.
    loopback.borrow_mut().host.refuse_size_state = Some(Errno::NotSupported);
    assert_eq!(
        client.set_size_state(window, WindowSizeState::Fullscreen),
        Err(Errno::NotSupported)
    );
    assert_eq!(loopback.borrow().host.size_states.len(), 1);
}

#[test]
fn an_owner_retitles_its_window_and_a_refusal_changes_nothing() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));

    let window = create_id(&mut client, 7, EVENTS_A, 1, "Files").expect("A creates");
    client
        .set_title(window, "Files - Documents")
        .expect("the owner retitles");
    assert_eq!(
        loopback.borrow().host.retitled,
        [(window, String::from("Files - Documents"))]
    );

    // A title the protocol refuses never reaches the session.
    let over_long = "t".repeat(WINDOW_TITLE_MAX + 1);
    assert_eq!(
        client.set_title(window, &over_long),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(
        client.set_title(window, "two\nlines"),
        Err(Errno::OutOfRange)
    );
    // An unknown window is refused, and a host refusal leaves the
    // previous title standing.
    client.set_title(window + 1, "ghost").expect("posted");
    assert_eq!(
        refusal(&mut client),
        Some(refused(window + 1, PostedRequest::Title, Errno::NotFound))
    );
    loopback.borrow_mut().host.refuse_retitle = Some(Errno::WouldBlock);
    client.set_title(window, "refused").expect("posted");
    assert_eq!(
        refusal(&mut client),
        Some(refused(window, PostedRequest::Title, Errno::WouldBlock))
    );
    assert_eq!(loopback.borrow().host.retitled.len(), 1);
}

#[test]
fn create_is_refused_fail_closed() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN), (8, FRAME_LEN - 1)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));

    // An unknown grant handle.
    assert_eq!(
        create_id(&mut client, 99, EVENTS_A, 1, "x"),
        Err(Errno::NotFound)
    );
    // A region too small for the frames it claims to hold.
    assert_eq!(
        create_id(&mut client, 8, EVENTS_A, 1, "x"),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(
        create_id(&mut client, 7, EVENTS_A, 3, "x"),
        Err(Errno::LengthOutOfRange)
    );
    // A kernel-domain caller is not a window client.
    loopback.borrow_mut().ticket = TICKET_KERNEL;
    assert_eq!(
        client.create(7, EVENTS_A, 1, &SURFACE, "x", WindowSizing::default()),
        Err(Errno::PermissionDenied)
    );
    // A caller the kernel cannot attest.
    loopback.borrow_mut().ticket = TICKET_UNATTESTED;
    assert_eq!(
        client.create(7, EVENTS_A, 1, &SURFACE, "x", WindowSizing::default()),
        Err(Errno::NotFound)
    );
    // Nothing leaked out of any refusal.
    let inner = loopback.borrow();
    assert_eq!(inner.server.window_count(), 0);
    assert!(inner.host.opened.is_empty());
}

/// The budget is derived from the machine, so a small board and a large
/// server get proportionate bounds from the same policy — and an
/// unanswered RAM query falls back to the display rather than to nothing.
#[test]
fn the_client_budget_scales_with_the_machine() {
    let screen = 1024 * 768 * 4;
    let small = client_frame_budget_bytes(1 << 30, screen); // 1 GiB
    let large = client_frame_budget_bytes(64 << 30, screen); // 64 GiB
    assert!(small > 0);
    assert_eq!(large, small * 64, "the bound follows the machine's RAM");
    // A screenful is the unit a window is measured in, so even the small
    // machine's bound holds a useful number of them.
    assert!(small > (screen as u64) * 8);
    // No RAM reading: bounded by the display instead of refusing every
    // window, and still bounded.
    let blind = client_frame_budget_bytes(0, screen);
    assert!(blind > 0);
    assert!(blind < small);
}

#[test]
fn a_client_is_bounded_by_the_bytes_it_holds_mapped() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));

    for _ in 0..FRAMES_PER_CLIENT {
        create_id(&mut client, 7, EVENTS_A, 1, "w").expect("within the budget");
    }
    assert_eq!(
        create_id(&mut client, 7, EVENTS_A, 1, "w"),
        Err(Errno::NoSpace)
    );
    // Another client still has its own budget.
    loopback.borrow_mut().ticket = TICKET_B;
    create_id(&mut client, 7, EVENTS_B, 1, "w").expect("B's own budget");
}

/// The bound is on bytes, so a client asking for *bigger* windows gets
/// correspondingly fewer of them — the thing a per-window count could not
/// express, and the reason it was the wrong bound.
#[test]
fn a_bigger_window_spends_more_of_the_same_budget() {
    const WIDE: DisplayMode = DisplayMode {
        width_px: 4,
        height_px: 3,
        stride_bytes: 16,
        format: DisplayFormat::Bgra8888,
    };
    // Four frames of this surface, so two windows fill the whole budget.
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN * 4)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    for _ in 0..2 {
        client
            .create(7, EVENTS_A, 4, &WIDE, "w", WindowSizing::default())
            .expect("within the budget");
    }
    assert_eq!(
        client.create(7, EVENTS_A, 4, &WIDE, "w", WindowSizing::default()),
        Err(Errno::NoSpace),
        "two four-frame windows spend what eight one-frame windows would"
    );
}

/// Releasing a window's frames is what makes a hidden window cost nothing:
/// the session unmaps its side, the client unmaps its own, and the pages go
/// because both did. Everything else about the window survives, and the next
/// paint re-attaches.
#[test]
fn released_frames_refuse_a_present_and_come_back_on_a_resize() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "w").expect("window");
    client
        .present(window, 0, one(DamageRect::full(&SURFACE)))
        .expect("presents");

    // The session's half of the release.
    let released = loopback.borrow_mut().server.release_frames(window);
    assert_eq!(released, FRAME_LEN as u64, "the mapped bytes are reported");
    assert_eq!(
        loopback.borrow().server.window_count(),
        1,
        "the window itself survives its pixels"
    );
    client
        .present(window, 0, one(DamageRect::full(&SURFACE)))
        .expect("posted");
    assert_eq!(
        refusal(&mut client),
        None,
        "a present with nothing attached is refused, which is no news to report"
    );
    assert!(
        client.content_released(window),
        "the refusal says the next present re-attaches"
    );
    // The client's own next paint re-attaches with an ordinary resize.
    client
        .resize(window, 8, 1, &SURFACE)
        .expect("a released window re-attaches");
    assert!(!client.content_released(window));
    client
        .present(window, 0, one(DamageRect::full(&SURFACE)))
        .expect("and presents again");
    assert_eq!(refusal(&mut client), None);
}

/// A released window holds no frames, so its bytes are free for another
/// window: what the client may hold is what it is actually holding.
#[test]
fn released_frames_stop_counting_against_the_client_budget() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let mut opened = Vec::new();
    for _ in 0..FRAMES_PER_CLIENT {
        opened.push(create_id(&mut client, 7, EVENTS_A, 1, "w").expect("within the budget"));
    }
    assert_eq!(
        create_id(&mut client, 7, EVENTS_A, 1, "w"),
        Err(Errno::NoSpace),
        "the budget is full"
    );
    let released = loopback.borrow_mut().server.release_frames(opened[0]);
    assert_eq!(released, FRAME_LEN as u64);
    create_id(&mut client, 7, EVENTS_A, 1, "w").expect("the released window's bytes are free");
}

/// Releasing twice, or releasing a window that never existed, changes
/// nothing: the session may report a release the client already handled.
#[test]
fn releasing_frames_is_idempotent_and_total() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "w").expect("window");
    assert_eq!(
        loopback.borrow_mut().server.release_frames(window),
        FRAME_LEN as u64
    );
    assert_eq!(
        loopback.borrow_mut().server.release_frames(window),
        0,
        "a second release has nothing left to give back"
    );
    assert_eq!(
        loopback.borrow_mut().server.release_frames(9999),
        0,
        "an unknown window is not an error"
    );
}

/// A resize is the other way a client grows what it holds mapped, and the
/// one a per-window count is blind to.
#[test]
fn a_resize_past_the_budget_is_refused_and_keeps_the_old_geometry() {
    // Eight times as tall as [`SURFACE`], so one frame of it is the client's
    // whole budget and two frames are twice it.
    const TALL: DisplayMode = DisplayMode {
        width_px: 4,
        height_px: 24,
        stride_bytes: 16,
        format: DisplayFormat::Bgra8888,
    };
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN * 8 * 2)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "w").expect("first window");

    client.resize(window, 8, 2, &TALL).expect("posted");
    assert_eq!(
        refusal(&mut client),
        Some(refused(window, PostedRequest::Resize, Errno::NoSpace)),
        "a resize is charged against the same budget as a create"
    );
    assert_eq!(client.take_owed(), Some(Ok(kept(window, &SURFACE))));
    // Resizing within the budget is still allowed, and the window keeps
    // working: the refusal above changed nothing.
    client
        .resize(window, 8, 1, &TALL)
        .expect("a resize inside the budget");
}

#[test]
fn a_refused_host_open_commits_nothing() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    loopback.borrow_mut().host.refuse_open = true;
    let mut client = WindowClient::new(Rc::clone(&loopback));

    assert_eq!(
        client.create(7, EVENTS_A, 1, &SURFACE, "x", WindowSizing::default()),
        Err(Errno::WouldBlock)
    );
    loopback.borrow_mut().host.refuse_open = false;
    // The refused create consumed no id.
    let window = create_id(&mut client, 7, EVENTS_A, 1, "x").expect("retry");
    assert_eq!(window, 1);
}

#[test]
fn present_bounds_are_enforced() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "x").expect("create");

    // A frame index past the created count.
    client.present(window, 2, full_damage()).expect("posted");
    assert_eq!(
        refusal(&mut client),
        Some(refused(window, PostedRequest::Present, Errno::OutOfRange))
    );
    // The refused frame never reached the screen, so the next present
    // declares the whole window whatever it changed itself.
    let corner = DamageRect {
        x: 0,
        y: 0,
        width_px: 1,
        height_px: 1,
    };
    client.present(window, 1, one(corner)).expect("posted");
    assert_eq!(refusal(&mut client), None);
    {
        let inner = loopback.borrow();
        let (_, _, seen) = inner.host.presented.last().expect("shown");
        assert_eq!(seen.rects(), full_damage().rects());
    }
    // Damage falling outside the surface.
    let outside = DamageRect {
        x: 3,
        y: 0,
        width_px: 2,
        height_px: 1,
    };
    client.present(window, 0, one(outside)).expect("posted");
    assert_eq!(
        refusal(&mut client),
        Some(refused(
            window,
            PostedRequest::Present,
            Errno::LengthOutOfRange
        ))
    );
    assert_eq!(loopback.borrow().host.presented.len(), 1);
}

#[test]
fn a_dead_clients_windows_are_torn_down_and_others_survive() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));

    let a1 = create_id(&mut client, 7, EVENTS_A, 1, "a1").expect("a1");
    let a2 = create_id(&mut client, 7, EVENTS_A, 1, "a2").expect("a2");
    loopback.borrow_mut().ticket = TICKET_B;
    let b1 = create_id(&mut client, 7, EVENTS_B, 1, "b1").expect("b1");

    {
        let inner = &mut *loopback.borrow_mut();
        inner.server.client_exited(&mut inner.host, proc_id(0xA1));
        assert_eq!(inner.server.window_count(), 1);
        assert_eq!(inner.host.closed, alloc::vec![a1, a2]);
    }
    // B's window still presents.
    client.present(b1, 0, full_damage()).expect("b1 lives");
}

#[test]
fn create_popup_round_trips_and_present_and_close_act_on_its_own_id() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));

    let parent = create_id(&mut client, 7, EVENTS_A, 1, "Files").expect("parent opens");
    let (popup, server) = client
        .create_popup(&popup_spec(parent, 8, EVENTS_A, 5, -7))
        .expect("popup opens");
    assert_eq!(popup, parent + 1);
    assert_eq!(
        server, SERVER,
        "a popup is stamped with the same session identity as a top-level create"
    );
    {
        let inner = loopback.borrow();
        assert_eq!(inner.server.window_count(), 2);
        // The popup went through the undecorated popup path, carrying its
        // parent and placement offsets — never the top-level `window_opened`
        // path, so it is never dressed with chrome or listed on the taskbar.
        assert_eq!(
            inner.host.popups,
            alloc::vec![(popup, parent, 5, -7, SURFACE)]
        );
        assert_eq!(
            inner.host.opened,
            alloc::vec![(
                proc_id(0xA1),
                parent,
                SURFACE,
                String::from("Files"),
                WindowSizing::default()
            )]
        );
    }

    // Present acts on the popup's own id exactly like a top-level window.
    client
        .present(popup, 0, full_damage())
        .expect("present into the popup");
    assert_eq!(loopback.borrow().host.presented[0].0, popup);

    // Closing the popup's own id tears down only the popup; the parent
    // survives.
    client.close(popup).expect("close the popup alone");
    {
        let inner = loopback.borrow();
        assert_eq!(inner.server.window_count(), 1);
        assert_eq!(inner.host.closed, alloc::vec![popup]);
    }
    client
        .present(parent, 0, full_damage())
        .expect("parent lives");
}

#[test]
fn a_popup_over_a_foreign_or_unknown_parent_is_refused() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));

    let a_window = create_id(&mut client, 7, EVENTS_A, 1, "A").expect("A's window");

    // A parent the caller does not own answers exactly like one that never
    // existed, leaking nothing about another client's windows.
    loopback.borrow_mut().ticket = TICKET_B;
    assert_eq!(
        client.create_popup(&popup_spec(a_window, 8, EVENTS_B, 0, 0)),
        Err(Errno::NotFound)
    );
    // An unknown parent id is refused the same way.
    loopback.borrow_mut().ticket = TICKET_A;
    assert_eq!(
        client.create_popup(&popup_spec(9999, 8, EVENTS_A, 0, 0)),
        Err(Errno::NotFound)
    );
    let inner = loopback.borrow();
    assert_eq!(inner.server.window_count(), 1);
    assert!(inner.host.popups.is_empty());
}

#[test]
fn a_popup_counts_against_the_same_per_client_budget() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));

    let parent = create_id(&mut client, 7, EVENTS_A, 1, "w").expect("first window");
    for _ in 1..FRAMES_PER_CLIENT {
        create_id(&mut client, 7, EVENTS_A, 1, "w").expect("within the budget");
    }
    // The client now holds its whole budget; a popup cannot be used to
    // exceed it.
    assert_eq!(
        client.create_popup(&popup_spec(parent, 8, EVENTS_A, 0, 0)),
        Err(Errno::NoSpace)
    );
    assert!(loopback.borrow().host.popups.is_empty());
}

#[test]
fn a_kernel_caller_cannot_open_a_popup() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let parent = create_id(&mut client, 7, EVENTS_A, 1, "w").expect("parent");

    loopback.borrow_mut().ticket = TICKET_KERNEL;
    assert_eq!(
        client.create_popup(&popup_spec(parent, 8, EVENTS_A, 0, 0)),
        Err(Errno::PermissionDenied)
    );
}

#[test]
fn a_refused_host_popup_commits_nothing() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN)]);
    let parent = {
        let mut client = WindowClient::new(Rc::clone(&loopback));
        let parent = create_id(&mut client, 7, EVENTS_A, 1, "w").expect("parent");
        loopback.borrow_mut().host.refuse_popup = true;
        assert_eq!(
            client.create_popup(&popup_spec(parent, 8, EVENTS_A, 0, 0)),
            Err(Errno::WouldBlock)
        );
        parent
    };
    // The refused popup consumed no id: the next window is the id the
    // popup would have taken.
    loopback.borrow_mut().host.refuse_popup = false;
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let next = create_id(&mut client, 7, EVENTS_A, 1, "w").expect("retry");
    assert_eq!(next, parent + 1);
    assert_eq!(loopback.borrow().server.window_count(), 2);
}

#[test]
fn closing_a_parent_tears_down_its_popups() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN), (9, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));

    let parent = create_id(&mut client, 7, EVENTS_A, 1, "parent").expect("parent");
    let (menu, _) = client
        .create_popup(&popup_spec(parent, 8, EVENTS_A, 0, 0))
        .expect("menu popup");
    let (sheet, _) = client
        .create_popup(&popup_spec(parent, 9, EVENTS_A, 0, 0))
        .expect("sheet popup");
    assert_eq!(loopback.borrow().server.window_count(), 3);

    // Closing the parent closes both popups keyed to it.
    client.close(parent).expect("close parent");
    let inner = loopback.borrow();
    assert_eq!(inner.server.window_count(), 0);
    assert!(inner.host.closed.contains(&parent));
    assert!(inner.host.closed.contains(&menu));
    assert!(inner.host.closed.contains(&sheet));
}

#[test]
fn a_dead_clients_parent_and_popups_are_all_torn_down() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));

    let parent = create_id(&mut client, 7, EVENTS_A, 1, "parent").expect("parent");
    let (popup, _) = client
        .create_popup(&popup_spec(parent, 8, EVENTS_A, 0, 0))
        .expect("popup");

    let inner = &mut *loopback.borrow_mut();
    inner.server.client_exited(&mut inner.host, proc_id(0xA1));
    assert_eq!(inner.server.window_count(), 0);
    assert!(inner.host.closed.contains(&parent));
    assert!(inner.host.closed.contains(&popup));
}

/// A fixed tool window of `parent` granted as `shm`, carried at `carry`.
fn tool_spec(parent_window_id: u64, shm: u64, carry: Option<u32>) -> ToolSpec {
    ToolSpec {
        transient: transient_spec(parent_window_id, shm, EVENTS_A),
        offset_x: -3,
        offset_y: 9,
        carry,
        title: tairix_abi::window_ipc::WindowTitle::new("Tools").expect("a title"),
        sizing: WindowSizing::Fixed,
    }
}

/// The range a resizable tool window of these tests may be resized within.
const TOOL_RANGE: WindowSizing = WindowSizing::Resizable {
    min_width_px: 2,
    min_height_px: 2,
    max_width_px: 8,
    max_height_px: 6,
};

#[test]
fn a_tool_window_opens_through_its_own_path_and_closes_alone() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let parent = create_id(&mut client, 7, EVENTS_A, 1, "Paint").expect("parent");
    let (tool, server) = client
        .create_tool(&tool_spec(parent, 8, Some(2)))
        .expect("tool window opens");
    assert_eq!(server, SERVER);
    {
        let inner = loopback.borrow();
        assert_eq!(
            inner.host.tools,
            alloc::vec![(
                tool,
                parent,
                Some(2),
                String::from("Tools"),
                WindowSizing::Fixed
            )]
        );
        assert!(inner.host.popups.is_empty(), "never the popup path");
        assert_eq!(inner.host.opened.len(), 1, "nor the top-level one");
    }
    client
        .present(tool, 0, full_damage())
        .expect("present into it");
    client.close(tool).expect("close it alone");
    assert_eq!(loopback.borrow().server.window_count(), 1);
    client
        .present(parent, 0, full_damage())
        .expect("the parent lives");
}

#[test]
fn a_transient_hangs_only_from_a_top_level_window_of_the_callers_own() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN), (9, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let parent = create_id(&mut client, 7, EVENTS_A, 1, "Paint").expect("parent");
    let (tool, _) = client
        .create_tool(&tool_spec(parent, 8, None))
        .expect("tool window");
    let (popup, _) = client
        .create_popup(&popup_spec(parent, 9, EVENTS_A, 0, 0))
        .expect("popup");
    // One level only: neither a tool window nor a popup hangs a transient.
    for transient in [tool, popup] {
        assert_eq!(
            client.create_tool(&tool_spec(transient, 9, None)),
            Err(Errno::NotSupported)
        );
        assert_eq!(
            client.create_popup(&popup_spec(transient, 9, EVENTS_A, 0, 0)),
            Err(Errno::NotSupported)
        );
    }
    loopback.borrow_mut().ticket = TICKET_B;
    let mut foreign = tool_spec(parent, 9, None);
    foreign.transient.event_endpoint = EVENTS_B;
    assert_eq!(
        client.create_tool(&foreign),
        Err(Errno::NotFound),
        "a foreign parent reads as none"
    );
    assert_eq!(loopback.borrow().server.window_count(), 3);
}

#[test]
fn a_dialog_opens_through_its_own_path_with_the_front_it_earned() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN), (9, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let owner = create_id(&mut client, 7, EVENTS_A, 1, "TextEdit").expect("owner");
    loopback.borrow_mut().host.focused = Some(owner);
    let (held, server) = client
        .create_dialog(&dialog_spec(owner, 8, DialogModality::OwnerModal))
        .expect("the dialog opens");
    assert_eq!(server, SERVER);
    loopback.borrow_mut().host.focused = None;
    let (free, _) = client
        .create_dialog(&dialog_spec(owner, 9, DialogModality::Modeless))
        .expect("so does a second");
    let inner = loopback.borrow();
    assert_eq!(
        inner.host.dialogs,
        alloc::vec![
            (
                held,
                owner,
                true,
                DialogModality::OwnerModal,
                String::from("Go to line")
            ),
            (
                free,
                owner,
                false,
                DialogModality::Modeless,
                String::from("Go to line")
            ),
        ],
        "the keyboard only where the user is working in the application"
    );
    assert!(inner.host.popups.is_empty() && inner.host.tools.is_empty());
    assert_eq!(inner.host.opened.len(), 1, "never the top-level path");
}

#[test]
fn a_dialog_hangs_only_from_a_top_level_window_of_the_callers_own() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN), (9, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let owner = create_id(&mut client, 7, EVENTS_A, 1, "Paint").expect("owner");
    let (dialog, _) = client
        .create_dialog(&dialog_spec(owner, 8, DialogModality::Modeless))
        .expect("a dialog");
    let (tool, _) = client
        .create_tool(&tool_spec(owner, 9, None))
        .expect("a tool window");
    for transient in [dialog, tool] {
        assert_eq!(
            client.create_dialog(&dialog_spec(transient, 9, DialogModality::OwnerModal)),
            Err(Errno::NotSupported),
            "one level only"
        );
    }
    assert_eq!(
        client.create_tool(&tool_spec(dialog, 9, None)),
        Err(Errno::NotSupported),
        "nor hangs anything else"
    );
    loopback.borrow_mut().ticket = TICKET_B;
    let mut foreign = dialog_spec(owner, 9, DialogModality::Modeless);
    foreign.transient.event_endpoint = EVENTS_B;
    assert_eq!(
        client.create_dialog(&foreign),
        Err(Errno::NotFound),
        "a foreign owner reads as none"
    );
    assert_eq!(loopback.borrow().server.window_count(), 3);
    assert_eq!(loopback.borrow().host.dialogs.len(), 1);
}

#[test]
fn a_dialog_shares_the_budget_and_closes_with_its_owner_and_what_holds_it() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let owner = create_id(&mut client, 7, EVENTS_A, 1, "Files").expect("owner");
    let (dialog, _) = client
        .create_dialog(&dialog_spec(owner, 8, DialogModality::Modeless))
        .expect("a dialog");
    for _ in 2..FRAMES_PER_CLIENT {
        create_id(&mut client, 7, EVENTS_A, 1, "w").expect("within the budget");
    }
    assert_eq!(
        client.create_dialog(&dialog_spec(owner, 8, DialogModality::Modeless)),
        Err(Errno::NoSpace)
    );
    let question = client
        .open_alert(EVENTS_A, Some(dialog), &sample_alert())
        .expect("a question holding the dialog");
    client.close(owner).expect("the owner closes");
    let inner = loopback.borrow();
    assert!(inner.host.closed.contains(&owner) && inner.host.closed.contains(&dialog));
    assert_eq!(inner.host.alerts_withdrawn, [question]);
    assert_eq!(
        alert_answers(&inner.sink, EVENTS_A),
        [WindowEvent::AlertClosed {
            alert_id: question,
            outcome: AlertOutcome::Dismissed
        }],
        "the question goes with the dialog it held"
    );
}

#[test]
fn a_refused_dialog_commits_nothing_and_spends_no_activation() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let owner = create_id(&mut client, 7, EVENTS_A, 1, "w").expect("owner");
    loopback.borrow_mut().host.focused = Some(owner);
    loopback.borrow_mut().host.refuse_popup = true;
    assert_eq!(
        client.create_dialog(&dialog_spec(owner, 8, DialogModality::OwnerModal)),
        Err(Errno::WouldBlock)
    );
    loopback.borrow_mut().host.refuse_popup = false;
    assert_eq!(loopback.borrow().server.window_count(), 1);
    let next = create_id(&mut client, 7, EVENTS_A, 1, "w").expect("retry");
    assert_eq!(next, owner + 1, "the refused dialog consumed no id");
}

#[test]
fn a_request_naming_another_applications_mailbox_is_refused() {
    // Every answer goes to the mailbox the caller binds for itself: a client
    // naming another's would have its events delivered into that one.
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    assert_eq!(
        create_id(&mut client, 7, EVENTS_B, 1, "Files"),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(
        client.open_alert(EVENTS_B, None, &sample_alert()),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(loopback.borrow().server.window_count(), 0);
    assert!(loopback.borrow().host.alerts.is_empty());
}

#[test]
fn the_kernel_sentinel_raises_no_alert() {
    let loopback = Loopback::with_regions(&[]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    loopback.borrow_mut().ticket = TICKET_KERNEL;
    assert_eq!(
        client.open_alert(EVENTS_A, None, &sample_alert()),
        Err(Errno::PermissionDenied)
    );
    assert!(loopback.borrow().host.alerts.is_empty());
}

#[test]
fn a_tool_window_shares_the_budget_and_goes_with_its_parent() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let parent = create_id(&mut client, 7, EVENTS_A, 1, "Paint").expect("parent");
    let (tool, _) = client
        .create_tool(&tool_spec(parent, 8, None))
        .expect("tool window");
    for _ in 2..FRAMES_PER_CLIENT {
        create_id(&mut client, 7, EVENTS_A, 1, "w").expect("within the budget");
    }
    assert_eq!(
        client.create_tool(&tool_spec(parent, 8, None)),
        Err(Errno::NoSpace)
    );
    client.close(parent).expect("close the parent");
    let inner = loopback.borrow();
    assert!(inner.host.closed.contains(&parent) && inner.host.closed.contains(&tool));
}

/// A tool window says at its create how it may be sized, and a resizable
/// one is told each size on its own id.
#[test]
fn a_resizable_tool_window_states_its_range_and_is_told_each_size_on_its_own_id() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN), (9, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let parent = create_id(&mut client, 7, EVENTS_A, 1, "Paint").expect("parent");
    let (palette, _) = client
        .create_tool(&ToolSpec {
            sizing: TOOL_RANGE,
            ..tool_spec(parent, 8, None)
        })
        .expect("a resizable palette");
    let (tools, _) = client
        .create_tool(&tool_spec(parent, 9, None))
        .expect("a fixed tool box");
    let resized = WindowEvent::Resized {
        window_id: palette,
        width_px: 6,
        height_px: 4,
        state: WindowSizeState::Restored,
    };
    let mut sink = QueueSink::default();
    {
        let inner = &mut *loopback.borrow_mut();
        let sizings: Vec<(u64, WindowSizing)> = inner
            .host
            .tools
            .iter()
            .map(|tool| (tool.0, tool.4))
            .collect();
        assert_eq!(
            sizings,
            [(palette, TOOL_RANGE), (tools, WindowSizing::Fixed)],
            "each says how it may be sized"
        );
        inner
            .server
            .deliver_event(&mut sink, &resized)
            .expect("routed");
    }
    let delivered: Vec<(u64, Result<WindowEvent, Errno>)> = sink
        .delivered
        .iter()
        .map(|(endpoint, frame)| (*endpoint, WindowEvent::from_bytes(frame)))
        .collect();
    assert_eq!(
        delivered,
        [(EVENTS_A, Ok(resized))],
        "its owner is told on the palette's own id"
    );
}

/// A tool window's range is restated as a window's is, and what the host
/// refuses — a change of kind — reaches the client and changes nothing.
#[test]
fn a_tool_window_s_range_is_restated_and_a_refused_kind_changes_nothing() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN), (9, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let parent = create_id(&mut client, 7, EVENTS_A, 1, "Paint").expect("parent");
    let (palette, _) = client
        .create_tool(&ToolSpec {
            sizing: TOOL_RANGE,
            ..tool_spec(parent, 8, None)
        })
        .expect("a resizable palette");
    let (tools, _) = client
        .create_tool(&tool_spec(parent, 9, None))
        .expect("a fixed tool box");
    let narrower = WindowSizing::Resizable {
        min_width_px: 2,
        min_height_px: 2,
        max_width_px: 6,
        max_height_px: 4,
    };
    client.set_sizing(palette, narrower).expect("posted");
    assert_eq!(loopback.borrow().host.resized_range, [(palette, narrower)]);
    loopback.borrow_mut().host.refuse_sizing = Some(Errno::NotSupported);
    client.set_sizing(tools, TOOL_RANGE).expect("posted");
    assert_eq!(
        refusal(&mut client),
        Some(refused(tools, PostedRequest::Sizing, Errno::NotSupported))
    );
    assert_eq!(loopback.borrow().host.resized_range.len(), 1);
}

#[test]
fn a_malformed_request_answers_a_typed_status_refusal() {
    let loopback = Loopback::with_regions(&[]);
    let mut reply = [0u8; WINDOW_REPLY_MAX];
    let inner = &mut *loopback.borrow_mut();
    let served = inner.server.serve(
        &mut inner.host,
        &mut inner.sink,
        &mut inner.identity,
        TICKET_A,
        &[0u8; 4],
        &mut reply,
    );
    let len = replied(served);
    assert_eq!(
        decode_status_reply(&reply[..len]),
        Err(Errno::BufferTooSmall)
    );
    let served = inner.server.serve(
        &mut inner.host,
        &mut inner.sink,
        &mut inner.identity,
        TICKET_A,
        &[0xFFu8; WindowRequest::MAX_WIRE_LEN],
        &mut reply,
    );
    let len = replied(served);
    assert_eq!(decode_status_reply(&reply[..len]), Err(Errno::BadMagic));
}

#[test]
fn events_reach_the_owning_endpoint_and_decode_through_the_client() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let a = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    loopback.borrow_mut().ticket = TICKET_B;
    let b = create_id(&mut client, 7, EVENTS_B, 1, "b").expect("b");

    let key = KeyInput::Pressed {
        key: KeyValue::Char('x'),
        modifiers: Modifiers::default(),
        physical: PhysicalKey::from_usage(0x1B),
    };
    let events = [
        WindowEvent::Focus {
            window_id: a,
            focused: true,
        },
        WindowEvent::Key { window_id: a, key },
        WindowEvent::Pointer {
            window_id: b,
            x: 3,
            y: 2,
            action: PointerAction::Pressed(PointerButtonCode::Primary),
            modifiers: Modifiers::default(),
            at_ns: 0,
        },
        WindowEvent::CloseRequested { window_id: a },
    ];
    let mut sink = QueueSink::default();
    {
        let inner = &mut *loopback.borrow_mut();
        for event in &events {
            inner
                .server
                .deliver_event(&mut sink, event)
                .expect("routed");
        }
    }
    // Each event reached its owner's endpoint.
    let endpoints: Vec<u64> = sink.delivered.iter().map(|(e, _)| *e).collect();
    assert_eq!(
        endpoints,
        alloc::vec![EVENTS_A, EVENTS_A, EVENTS_B, EVENTS_A]
    );

    // The app-side wait decodes exactly what was routed to it.
    let queue: VecDeque<[u8; WindowEvent::WIRE_LEN]> = sink
        .delivered
        .iter()
        .filter(|(endpoint, _)| *endpoint == EVENTS_A)
        .map(|(_, frame)| *frame)
        .collect();
    let mut waiter = WindowEvents::new(QueueSource::new(queue));
    assert_eq!(
        waiter.wait(&mut client),
        Ok(Some(WindowEvent::Focus {
            window_id: a,
            focused: true
        }))
    );
    assert_eq!(
        waiter.wait(&mut client),
        Ok(Some(WindowEvent::Key { window_id: a, key }))
    );
    assert_eq!(
        waiter.wait(&mut client),
        Ok(Some(WindowEvent::CloseRequested { window_id: a }))
    );
    // An empty queue surfaces the source's wait condition, never a
    // fabricated event — as a mailbox failure, which is what ends a channel.
    assert_eq!(
        waiter.wait(&mut client),
        Err(EventError::Mailbox(Errno::WouldBlock))
    );
}

/// The parked wait drains before it parks: a loop that has queued input must
/// never be made to wait for a wake that has already been consumed.
///
/// The park is what a loop interleaving decode work with input has to avoid
/// entering while input is queued, so the ordering is asserted rather than
/// assumed.
#[test]
fn the_parked_wait_drains_before_it_parks() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let queued = WindowEvent::CloseRequested { window_id: 11 };
    let source = QueueSource::new([queued.to_le_bytes()]);
    let parked = Rc::clone(&source.parked);
    let mut waiter = WindowEvents::new(source);

    assert_eq!(waiter.wait(&mut client), Ok(Some(queued)));
    assert_eq!(parked.get(), 0, "a queued event must not cost a park");

    // Nothing left: the drain reports empty and the park is entered, which is
    // how a genuinely idle loop stops running.
    assert_eq!(
        waiter.wait(&mut client),
        Err(EventError::Mailbox(Errno::WouldBlock))
    );
    assert_eq!(parked.get(), 1);
}

/// `try_wait` reports an empty mailbox as `None` without parking, and hands
/// back exactly the events `wait` would.
#[test]
fn the_drained_wait_answers_without_parking() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");

    let queued = WindowEvent::Focus {
        window_id: window,
        focused: true,
    };
    let mut waiter = WindowEvents::new(QueueSource::new([queued.to_le_bytes()]));
    assert_eq!(waiter.try_wait(&mut client), Ok(Some(queued)));
    assert_eq!(
        waiter.try_wait(&mut client),
        Ok(None),
        "an empty mailbox is answered at once, never parked on"
    );
}

/// A drain that fails with the one code a decode also produces, so a reader
/// that told the two apart by [`Errno`] alone could not.
struct FailingDrain;

impl EventDrain for FailingDrain {
    fn try_next(&mut self, _event: &mut [u8; WindowEvent::WIRE_LEN]) -> Result<bool, Errno> {
        Err(Errno::LengthOutOfRange)
    }
}

/// A failed drain is answered as a *mailbox* failure even when its code is one
/// a decode refusal also uses.
///
/// The caller's two answers are opposites — read on past an undecodable frame,
/// end the channel on a dead mailbox — so an ambiguous one is a spin: the next
/// read meets the same failure and the loop never parks. `ipc_recv` really can
/// answer `LengthOutOfRange` (a received length the address width cannot
/// hold), so this is not a hypothetical code.
#[test]
fn a_failed_drain_is_never_mistaken_for_an_undecodable_frame() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let mut events = WindowEvents::new(FailingDrain);

    assert_eq!(
        events.try_wait(&mut client),
        Err(EventError::Mailbox(Errno::LengthOutOfRange))
    );
}

/// A frame the session sent that will not decode is answered as *undecodable*,
/// so the caller reads past it rather than tearing the channel down.
#[test]
fn a_frame_that_will_not_decode_is_answered_apart_from_a_dead_mailbox() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let mut events = WindowEvents::new(DrainOnlySource {
        queue: [[0u8; WindowEvent::WIRE_LEN]].into_iter().collect(),
    });

    assert!(matches!(
        events.try_wait(&mut client),
        Err(EventError::Undecodable(_))
    ));
    assert_eq!(
        events.try_wait(&mut client),
        Ok(None),
        "the refused frame is consumed, so the next read moves past it"
    );
}

/// A mailbox with no park of its own: the shape an app takes when its loop
/// dispatches several wake sources and a frame deadline itself, so parking is
/// the loop's business and only the drain is the mailbox's.
struct DrainOnlySource {
    queue: VecDeque<[u8; WindowEvent::WIRE_LEN]>,
}

impl EventDrain for DrainOnlySource {
    fn try_next(&mut self, event: &mut [u8; WindowEvent::WIRE_LEN]) -> Result<bool, Errno> {
        let Some(frame) = self.queue.pop_front() else {
            return Ok(false);
        };
        *event = frame;
        Ok(true)
    }
}

/// An app whose loop owns its park still reads through the shared stream, and
/// so still gets the fold.
///
/// This is the terminal emulator's shape, and the reason it matters: its
/// wait-set carries a shell stream and a child per window, a settings
/// worker, a pressure wake, and an animation deadline, so it dispatches its
/// own wakes and cannot hand the park to a source. When the stream was
/// reachable only *through* a park, that loop had to spell a raw mailbox
/// drain of its own — which folded nothing, so a resize-grab's queued samples
/// were each applied on release and walked the window back through the drag.
#[test]
fn a_loop_that_owns_its_park_still_folds_a_resize_run() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");

    let resized = |width_px, height_px| WindowEvent::Resized {
        window_id: window,
        width_px,
        height_px,
        state: WindowSizeState::Restored,
    };
    // What a drag out and back in leaves queued: every sample the pointer
    // produced, ending on the size the window actually settled at.
    let drag = [
        resized(400, 300),
        resized(520, 380),
        resized(640, 460),
        resized(520, 380),
        resized(430, 310),
    ];
    let mut events = WindowEvents::new(DrainOnlySource {
        queue: drag.iter().map(WindowEvent::to_le_bytes).collect(),
    });

    assert_eq!(
        events.try_wait(&mut client),
        Ok(Some(resized(430, 310))),
        "the whole drag collapses onto the size it ended at, so the window is \
         re-laid-out once instead of replaying every sample behind the pointer"
    );
    assert_eq!(
        events.try_wait(&mut client),
        Ok(None),
        "and nothing of the run is left to apply afterwards"
    );
}

/// A run of resizes for one window folds to its newest: an interactive
/// resize-grab reports one per pointer sample, and an app that re-laid-out
/// for each would do the work once per queued sample to reach the size the
/// last one already named.
#[test]
fn a_run_of_resizes_folds_to_the_newest_extent() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");

    let resized = |width_px, height_px| WindowEvent::Resized {
        window_id: window,
        width_px,
        height_px,
        state: WindowSizeState::Restored,
    };
    let mut waiter = WindowEvents::new(QueueSource::new([
        resized(100, 50).to_le_bytes(),
        resized(120, 60).to_le_bytes(),
        resized(140, 70).to_le_bytes(),
    ]));
    assert_eq!(
        waiter.try_wait(&mut client),
        Ok(Some(resized(140, 70))),
        "the whole run collapses onto the size the last sample named"
    );
    assert_eq!(waiter.try_wait(&mut client), Ok(None));
}

/// A present is posted, and the next waits only for its answer — the frame
/// the first named is the session's until then — which comes at once when
/// nothing waited to be shown.
#[test]
fn a_present_is_posted_and_the_next_waits_only_for_its_answer() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    loopback.borrow_mut().holds_posts = true;

    client.present(window, 0, full_damage()).expect("posted");
    assert!(
        loopback.borrow().awaited.is_empty(),
        "the first waits on nothing"
    );
    assert!(
        loopback.borrow().host.presented.is_empty(),
        "nor is it served"
    );

    client.present(window, 1, full_damage()).expect("posted");
    let inner = loopback.borrow();
    assert_eq!(
        inner.awaited.len(),
        1,
        "the second waited for the first alone"
    );
    assert_eq!(
        inner.host.presented.len(),
        1,
        "the first was answered before the second went"
    );
    assert_eq!(
        inner.unserved.len(),
        1,
        "the second waits on nothing in turn"
    );
}

/// However often a window presents between the desktop's frames, the session
/// converts one frame — the newest — carrying every change since the last.
#[test]
fn presents_between_frames_are_shown_once_with_every_change() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    loopback.borrow_mut().holds_frames = true;
    let first = DamageRect {
        x: 0,
        y: 0,
        width_px: 1,
        height_px: 1,
    };
    let second = DamageRect {
        x: 3,
        y: 2,
        width_px: 1,
        height_px: 1,
    };

    client.present(window, 0, one(first)).expect("posted");
    client.present(window, 1, one(second)).expect("posted");
    assert!(loopback.borrow().server.has_pending_frames());
    loopback.borrow_mut().latch();
    {
        let inner = loopback.borrow();
        assert_eq!(
            inner.host.presented.len(),
            1,
            "one conversion for two presents"
        );
        let (id, frame, seen) = &inner.host.presented[0];
        assert_eq!(*id, window);
        let newest: Vec<u8> = (FRAME_LEN..2 * FRAME_LEN)
            .map(|i| region_byte(7, i))
            .collect();
        assert_eq!(frame, &newest, "the newest frame is the one read");
        assert_eq!(seen.rects(), [first, second]);
    }
    assert!(!loopback.borrow().server.has_pending_frames());
    loopback.borrow_mut().latch();
    assert_eq!(
        loopback.borrow().host.presented.len(),
        1,
        "a frame shown is not shown again"
    );
}

/// A re-map or a release drops the frame waiting to be shown: it names a frame
/// of a region the session no longer reads.
#[test]
fn a_resize_or_a_release_drops_the_pending_frame() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN), (8, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    loopback.borrow_mut().holds_frames = true;

    client.present(window, 0, full_damage()).expect("posted");
    client.resize(window, 8, 2, &SURFACE).expect("posted");
    assert_eq!(client.take_owed(), None, "the re-map was accepted");
    assert!(!loopback.borrow().server.has_pending_frames());

    client.present(window, 0, full_damage()).expect("posted");
    loopback.borrow_mut().server.release_frames(window);
    assert!(!loopback.borrow().server.has_pending_frames());
    loopback.borrow_mut().latch();
    assert!(loopback.borrow().host.presented.is_empty());
}

/// A frame the compositor cannot show is reported once however often it is
/// tried again, and what it changed is shown with the next frame that shows.
#[test]
fn a_frame_the_host_cannot_show_is_reported_once_and_owed_to_the_next() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    {
        let inner = &mut *loopback.borrow_mut();
        inner.holds_frames = true;
        inner.host.refuse_frames = Some(Errno::OutOfMemory);
    }
    let first = DamageRect {
        x: 0,
        y: 0,
        width_px: 1,
        height_px: 1,
    };
    let second = DamageRect {
        x: 3,
        y: 2,
        width_px: 1,
        height_px: 1,
    };
    client.present(window, 0, one(first)).expect("posted");
    loopback.borrow_mut().latch();
    loopback.borrow_mut().latch();
    {
        let inner = &mut *loopback.borrow_mut();
        assert_eq!(inner.host.frames_refused, [(window, Errno::OutOfMemory)]);
        assert!(inner.host.presented.is_empty());
        assert!(!inner.server.has_pending_frames());
        inner.host.refuse_frames = None;
    }
    client.present(window, 1, one(second)).expect("posted");
    loopback.borrow_mut().latch();
    let inner = loopback.borrow();
    let seen: Vec<&[DamageRect]> = inner
        .host
        .presented
        .iter()
        .map(|(.., seen)| seen.rects())
        .collect();
    assert_eq!(
        seen,
        [&[first, second][..]],
        "the refused frame's change is shown"
    );
}

/// The library's answer to a redraw follows a present still in flight rather
/// than waiting for it: it runs inside the application's event drain.
#[test]
fn a_redraw_asked_with_a_present_in_flight_follows_it_without_waiting() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    loopback.borrow_mut().holds_posts = true;
    let corner = DamageRect {
        x: 0,
        y: 0,
        width_px: 1,
        height_px: 1,
    };
    client.present(window, 0, one(corner)).expect("posted");

    assert_eq!(client.answer_redraw(window), Ok(true));
    assert!(
        loopback.borrow().awaited.is_empty(),
        "the answer never waits"
    );
    loopback.borrow_mut().serve_posted();
    client.reap_posted();
    loopback.borrow_mut().serve_posted();
    let inner = loopback.borrow();
    let damages: Vec<DamageList> = inner
        .host
        .presented
        .iter()
        .map(|(.., seen)| *seen)
        .collect();
    assert_eq!(
        damages,
        [one(corner), full_damage()],
        "the redraw went once the present before it was answered"
    );
}

/// A window whose frames the session released holds nothing the library could
/// re-send, so its redraw is the application's own next paint.
#[test]
fn a_released_window_is_left_for_its_application_to_paint() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    client.present(window, 0, full_damage()).expect("posted");
    loopback.borrow_mut().server.release_frames(window);

    let released = WindowEvent::ContentReleased { window_id: window };
    let mut events = WindowEvents::new(QueueSource::new([released.to_le_bytes()]));
    assert_eq!(events.try_wait(&mut client), Ok(Some(released)));
    assert!(client.content_released(window));
    let sent = loopback.borrow().sent.len();
    assert_eq!(client.answer_redraw(window), Ok(false));
    assert_eq!(loopback.borrow().sent.len(), sent, "nothing is sent");
}

/// A released window is hidden only until the session asks for it again: from
/// then it is painted, and its next present re-attaches the frames — an app
/// that held its paint for as long as the region stayed released never
/// painted the window again.
#[test]
fn a_released_window_is_hidden_until_the_session_asks_for_it_again() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN), (8, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    client.present(window, 0, full_damage()).expect("posted");
    assert!(!client.is_hidden(window));

    loopback.borrow_mut().server.release_frames(window);
    let released = WindowEvent::ContentReleased { window_id: window };
    let asked = WindowEvent::RedrawRequested { window_id: window };
    let mut events = WindowEvents::new(QueueSource::new([
        released.to_le_bytes(),
        asked.to_le_bytes(),
    ]));
    assert_eq!(events.try_wait(&mut client), Ok(Some(released)));
    assert!(client.is_hidden(window), "nobody can see it");

    assert_eq!(events.try_wait(&mut client), Ok(Some(asked)));
    assert!(!client.is_hidden(window), "asked for, it is painted");
    assert!(
        client.content_released(window),
        "and its next present re-attaches"
    );
    client.resize(window, 8, 2, &SURFACE).expect("re-attached");
    assert!(!client.content_released(window) && !client.is_hidden(window));
}

/// A present this process's own calls leave no room for goes as a call, which
/// waits for the room, rather than waiting on nothing.
#[test]
fn a_present_with_no_room_goes_as_a_call() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    {
        let inner = &mut *loopback.borrow_mut();
        inner.holds_posts = true;
        inner.lane = Some(1);
    }
    client
        .set_cursor(window, CursorShape::Text)
        .expect("posted");
    client.present(window, 0, full_damage()).expect("called");
    let inner = loopback.borrow();
    assert!(inner.awaited.is_empty());
    assert_eq!(inner.host.presented.len(), 1, "answered before it returned");
    assert_eq!(inner.host.cursors, [(window, CursorShape::Text)]);
}

/// A refused frame is tried again with the next frame the desktop composes,
/// asking for none of its own, and is reported once until the window next
/// shows one: a window whose first frame was refused would otherwise stay off
/// screen until its application happened to present again.
#[test]
fn a_refused_frame_is_tried_again_with_the_next_desktop_frame() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    {
        let inner = &mut *loopback.borrow_mut();
        inner.holds_frames = true;
        inner.host.refuse_frames = Some(Errno::OutOfMemory);
    }
    client.present(window, 0, full_damage()).expect("posted");
    loopback.borrow_mut().latch();
    client.present(window, 1, full_damage()).expect("posted");
    loopback.borrow_mut().latch();
    {
        let inner = &mut *loopback.borrow_mut();
        assert_eq!(inner.host.frames_refused, [(window, Errno::OutOfMemory)]);
        assert!(
            !inner.server.has_pending_frames(),
            "a refused frame asks the desktop for no frame of its own"
        );
        inner.host.refuse_frames = None;
    }
    loopback.borrow_mut().latch();
    assert_eq!(
        loopback.borrow().host.presented.len(),
        1,
        "shown with the next frame, no present needed"
    );

    loopback.borrow_mut().host.refuse_frames = Some(Errno::OutOfMemory);
    client.present(window, 0, full_damage()).expect("posted");
    loopback.borrow_mut().latch();
    assert_eq!(
        loopback.borrow().host.frames_refused.len(),
        2,
        "refused again after showing, it is reported again"
    );
}

/// A closed window's frame asks the desktop for no frame.
#[test]
fn a_closed_windows_frame_asks_for_no_desktop_frame() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    loopback.borrow_mut().holds_frames = true;
    client.present(window, 0, full_damage()).expect("posted");
    assert!(loopback.borrow().server.has_pending_frames());
    client.close(window).expect("closed");
    assert!(!loopback.borrow().server.has_pending_frames());
}

/// Present twice to `window`, the second held behind the frame the first
/// named, which the desktop has yet to show.
fn hold_a_present(client: &mut WindowClient<Rc<RefCell<Loopback>>>, window: u64) {
    client.present(window, 0, full_damage()).expect("posted");
    client.present(window, 1, full_damage()).expect("posted");
    assert!(
        client.present_in_flight(window),
        "held behind an unshown frame"
    );
}

/// The tickets the engine answered through `send_answers` that no post was
/// waiting on, each `Ok`: the presents they answer were recorded.
fn called_tickets(loopback: &Loopback) -> Vec<u64> {
    loopback
        .called
        .iter()
        .map(|(ticket, reply)| {
            assert_eq!(
                decode_status_reply(reply),
                Ok(()),
                "recorded, so answered Ok"
            );
            *ticket
        })
        .collect()
}

/// A present replacing a frame the desktop has yet to show is answered once
/// that frame is shown; one after a shown frame is answered at once.
#[test]
fn a_present_replacing_an_unshown_frame_is_answered_once_it_is_shown() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    loopback.borrow_mut().holds_frames = true;

    client.present(window, 0, full_damage()).expect("posted");
    assert!(
        !client.present_in_flight(window),
        "nothing waited to be shown"
    );
    client.present(window, 1, full_damage()).expect("posted");
    assert!(
        client.present_in_flight(window),
        "its answer waits for the frame it replaced"
    );
    loopback.borrow_mut().latch();
    assert!(!client.present_in_flight(window), "shown, it is answered");
    client.present(window, 0, full_damage()).expect("posted");
    assert!(
        !client.present_in_flight(window),
        "after a shown frame it is answered at once"
    );
}

/// A client presenting as fast as it can draw is held to the desktop's frames:
/// for each one it draws at most two, and the desktop converts one. Answered as
/// each was recorded, a loop of sixty-four presents drew all sixty-four for the
/// one frame the desktop showed.
#[test]
fn a_streaming_client_is_held_to_the_desktops_frames() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    let latched = {
        let inner = &mut *loopback.borrow_mut();
        inner.holds_frames = true;
        inner.latches
    };
    for frame_index in 0..64u32 {
        client
            .present(window, frame_index % 2, full_damage())
            .expect("presented");
    }
    let inner = loopback.borrow();
    let frames = inner.latches - latched;
    assert_eq!(
        frames, 31,
        "every second present waited for a desktop frame"
    );
    assert_eq!(inner.host.presented.len(), frames, "one conversion a frame");
}

/// A resize, a release and a close each answer the present held behind the
/// frame they drop.
#[test]
fn a_resize_a_release_or_a_close_answers_the_present_it_supersedes() {
    let regions = [(7, 2 * FRAME_LEN), (8, 2 * FRAME_LEN), (9, 2 * FRAME_LEN)];
    let loopback = Loopback::with_regions(&regions);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    let other = create_id(&mut client, 9, EVENTS_A, 2, "b").expect("b");
    loopback.borrow_mut().holds_frames = true;

    hold_a_present(&mut client, window);
    client.resize(window, 8, 2, &SURFACE).expect("posted");
    assert!(!client.present_in_flight(window), "the re-map answered it");

    hold_a_present(&mut client, window);
    {
        let inner = &mut *loopback.borrow_mut();
        inner.server.release_frames(window);
        inner.collect_answers();
    }
    assert!(!client.present_in_flight(window), "the release answered it");

    hold_a_present(&mut client, other);
    client.close(other).expect("closed");
    assert!(
        loopback.borrow().withheld.is_empty(),
        "the close answered it"
    );
}

/// A frame the desktop cannot show still answers the present held behind it,
/// which was recorded all the same.
#[test]
fn a_refused_frame_still_answers_the_present_held_behind_it() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    {
        let inner = &mut *loopback.borrow_mut();
        inner.holds_frames = true;
        inner.host.refuse_frames = Some(Errno::OutOfMemory);
    }
    hold_a_present(&mut client, window);
    loopback.borrow_mut().latch();
    assert!(!client.present_in_flight(window), "refused yet answered");
    assert_eq!(
        loopback.borrow().host.frames_refused,
        [(window, Errno::OutOfMemory)]
    );
}

/// A present replacing a frame whose answer is held answers the older at once
/// and is held in its place, so a window never holds two.
#[test]
fn a_present_superseding_a_held_one_answers_the_older_at_once() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    let inner = &mut *loopback.borrow_mut();
    inner.holds_frames = true;
    let (_, first) = inner.serve_request(&present_frame(window, 0));
    assert!(first.is_some(), "nothing waited to be shown");
    let (older, held) = inner.serve_request(&present_frame(window, 1));
    assert!(held.is_none(), "held behind the unshown frame");
    let (newer, held) = inner.serve_request(&present_frame(window, 0));
    assert!(held.is_none(), "held in the older's place");
    inner.collect_answers();
    assert_eq!(
        called_tickets(inner),
        [older],
        "the older is answered at once"
    );
    inner.latch();
    assert_eq!(called_tickets(inner), [older, newer]);
}

/// A client that exits is owed nothing more: its calls ended with it, so what
/// it was owed, held or due, is dropped unsent.
#[test]
fn an_exited_clients_answers_are_dropped() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    let inner = &mut *loopback.borrow_mut();
    inner.holds_frames = true;
    for frame_index in [0, 1, 0] {
        let _ = inner.serve_request(&present_frame(window, frame_index));
    }
    inner.server.client_exited(&mut inner.host, proc_id(0xA1));
    assert!(!inner.server.holds_answers());
    inner.collect_answers();
    assert!(
        inner.called.is_empty(),
        "nothing is sent to a client that has gone"
    );
}

/// Every call the engine is handed is answered exactly once, however its
/// answer falls due: as it is served, superseded, at a latch, a resize or a
/// close.
#[test]
fn every_call_is_answered_exactly_once() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN), (8, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    let inner = &mut *loopback.borrow_mut();
    inner.holds_frames = true;
    let resize = request_frame(&WindowRequest::Resize {
        window_id: window,
        shm_handle: 8,
        frame_count: 2,
        width_px: SURFACE.width_px,
        height_px: SURFACE.height_px,
        stride_bytes: SURFACE.stride_bytes,
        format: SURFACE.format,
    });
    let close = request_frame(&WindowRequest::Close { window_id: window });
    // `None` is a desktop frame.
    let script = [
        Some(present_frame(window, 0)),
        Some(present_frame(window, 1)),
        Some(present_frame(window, 0)),
        None,
        Some(present_frame(window, 1)),
        Some(present_frame(window, 0)),
        Some(resize),
        Some(present_frame(window, 0)),
        Some(present_frame(window, 1)),
        Some(present_frame(window, 0)),
        Some(close),
    ];
    let mut served = Vec::new();
    let mut answered = Vec::new();
    for step in script {
        match step {
            Some(request) => {
                let (ticket, reply) = inner.serve_request(&request);
                served.push(ticket);
                answered.extend(reply.map(|_| ticket));
            }
            None => inner.latch(),
        }
        inner.collect_answers();
    }
    answered.extend(called_tickets(inner));
    answered.sort_unstable();
    assert_eq!(answered, served, "each call answered, and only once");
}

/// While no frame reaches the screen the session answers what is held without
/// showing it, so no client stalls behind a dark screen or an uncovered lock;
/// the newest frame waits for a screen that can show it.
#[test]
fn held_answers_are_released_with_their_frames_left_to_show() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    loopback.borrow_mut().holds_frames = true;
    hold_a_present(&mut client, window);
    {
        let inner = &mut *loopback.borrow_mut();
        assert!(inner.server.holds_answers());
        inner.server.release_answers();
        inner.collect_answers();
        assert!(!inner.server.holds_answers());
        assert!(
            inner.server.has_pending_frames(),
            "the frame waits for a screen"
        );
    }
    assert!(
        !client.present_in_flight(window),
        "answered with nothing shown"
    );
    client.present(window, 0, full_damage()).expect("posted");
    assert!(
        client.present_in_flight(window),
        "the next is held to the desktop's pace"
    );
    loopback.borrow_mut().latch();
    assert_eq!(
        loopback.borrow().host.presented.len(),
        1,
        "one frame shown, the newest"
    );
}

/// A resize drops the presents not yet sent for the region it replaces: the
/// redraw held behind a present in flight names a frame of the old region, so
/// sent after the resize it would show the new region unpainted, or be refused
/// as damage the window no longer has.
#[test]
fn a_resize_drops_a_redraw_held_for_the_region_it_replaced() {
    const SMALL: DisplayMode = DisplayMode {
        width_px: 2,
        height_px: 3,
        stride_bytes: 8,
        format: DisplayFormat::Bgra8888,
    };
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN), (8, 2 * 24)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    loopback.borrow_mut().holds_posts = true;
    client.present(window, 0, full_damage()).expect("posted");
    assert_eq!(client.answer_redraw(window), Ok(true), "held behind it");
    client.resize(window, 8, 2, &SMALL).expect("posted");

    loopback.borrow_mut().serve_posted();
    client.reap_posted();
    loopback.borrow_mut().serve_posted();
    {
        let host = &loopback.borrow().host;
        assert_eq!(host.presented.len(), 1, "only the present made before it");
        assert_eq!(host.resized.len(), 1);
    }
    assert_eq!(client.take_owed(), None, "and nothing was refused");
}

/// A resize while the window is hidden leaves it hidden: nobody can see it
/// until the session asks for it again, so nothing is painted at the new size
/// before then.
#[test]
fn a_resize_while_hidden_keeps_the_window_hidden() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN), (8, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    client.present(window, 0, full_damage()).expect("posted");
    loopback.borrow_mut().server.release_frames(window);
    let released = WindowEvent::ContentReleased { window_id: window };
    let mut events = WindowEvents::new(QueueSource::new([released.to_le_bytes()]));
    assert_eq!(events.try_wait(&mut client), Ok(Some(released)));

    client.resize(window, 8, 2, &SURFACE).expect("posted");
    assert!(client.is_hidden(window), "nobody can see it yet");
}

/// A re-attach refused for room leaves the window released and, once room is
/// made, asks for its paint again: a window the session never sized has no
/// size to deliver back in its place.
#[test]
fn a_reattach_refused_for_room_asks_for_the_paint_again() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN), (8, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    client.present(window, 0, full_damage()).expect("posted");
    loopback.borrow_mut().server.release_frames(window);
    let released = WindowEvent::ContentReleased { window_id: window };
    let asked = WindowEvent::RedrawRequested { window_id: window };
    let mut events = WindowEvents::new(QueueSource::new([
        released.to_le_bytes(),
        asked.to_le_bytes(),
    ]));
    assert_eq!(events.try_wait(&mut client), Ok(Some(released)));
    assert_eq!(events.try_wait(&mut client), Ok(Some(asked)));
    {
        let mut inner = loopback.borrow_mut();
        inner.holds_posts = true;
        inner.lane = Some(1);
    }
    client
        .set_cursor(window, CursorShape::Text)
        .expect("fills the lane");
    assert_eq!(
        client.resize(window, 8, 2, &SURFACE),
        Err(Errno::WouldBlock)
    );
    assert!(client.content_released(window), "still released");
    assert_eq!(
        events.try_wait(&mut client),
        Ok(None),
        "nothing answered yet"
    );

    loopback.borrow_mut().serve_posted();
    assert_eq!(
        events.try_wait(&mut client),
        Ok(Some(asked)),
        "its paint is asked for again once room is made"
    );
}

/// A frame is drawn only once the window's previous present is answered —
/// until then the session may still read it — and a re-mapped ring starts
/// again from its first frame.
#[cfg(feature = "rt")]
#[test]
fn a_frame_is_drawn_only_once_the_previous_present_is_answered() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    loopback.borrow_mut().holds_posts = true;
    let mut ring = crate::frames::FrameRing::new(&SURFACE);
    let mut drawn = Vec::new();
    for remapped in [false, true, false] {
        if remapped {
            ring.reset(&SURFACE);
        }
        ring.present(&mut client, window, full_damage(), |index, _| {
            drawn.push((index, loopback.borrow().unserved.len()));
            Ok(())
        })
        .expect("presented");
    }
    assert_eq!(
        drawn,
        [(0, 0), (0, 0), (1, 0)],
        "each frame drawn with nothing in flight, a re-mapped ring from its first"
    );
}

/// Waiting for a window's present reaps nothing else: each present is awaited
/// by its own answer, so a window with none in flight costs no call.
#[test]
fn awaiting_a_present_reaps_nothing_else() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    loopback.borrow_mut().holds_posts = true;
    client
        .set_cursor(window, CursorShape::Text)
        .expect("posted");
    let reaps = loopback.borrow().reaps;
    client.await_present(window).expect("none in flight");
    client.present(window, 0, full_damage()).expect("posted");
    assert_eq!(loopback.borrow().reaps, reaps, "nothing was reaped");
}

/// A present whose wait ends unanswered while the call is still live stays
/// posted for the reap: forgotten, its answer would leave the window's reply
/// member ready for ever and every park would return at once. No second
/// present goes out while it is in flight.
#[test]
fn a_present_whose_wait_is_interrupted_stays_posted_for_the_reap() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    loopback.borrow_mut().holds_posts = true;
    client.present(window, 0, full_damage()).expect("posted");
    loopback.borrow_mut().refuses_awaits = Some(Errno::Interrupted);
    assert_eq!(client.await_present(window), Err(Errno::Interrupted));
    let sent = loopback.borrow().sent.len();
    assert_eq!(
        client.present(window, 1, full_damage()),
        Err(Errno::Interrupted)
    );
    assert_eq!(loopback.borrow().sent.len(), sent, "no second present");
    assert!(client.present_in_flight(window), "still posted");
    {
        let mut inner = loopback.borrow_mut();
        inner.refuses_awaits = None;
        inner.serve_posted();
    }
    assert!(!client.present_in_flight(window), "its answer was taken in");
    client
        .present(window, 1, full_damage())
        .expect("the next present");
}

/// A present whose wait ends after the call itself has ended is retired, so
/// the window is free for its next present.
#[test]
fn a_present_whose_call_ended_unanswered_is_retired() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    loopback.borrow_mut().holds_posts = true;
    client.present(window, 0, full_damage()).expect("posted");
    {
        let mut inner = loopback.borrow_mut();
        inner.unserved.clear();
        inner.refuses_awaits = Some(Errno::TimedOut);
    }
    assert_eq!(client.await_present(window), Ok(()));
    assert!(!client.present_in_flight(window));
}

/// No per-input request waits on the session: each is posted and answered
/// later, so a loop busy with a slow desktop still returns at once.
#[test]
fn a_posted_request_never_waits_on_the_session() {
    const BIG: DisplayMode = DisplayMode {
        width_px: 8,
        height_px: 3,
        stride_bytes: 32,
        format: DisplayFormat::Bgra8888,
    };
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (20, 2 * 96)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    loopback.borrow_mut().holds_posts = true;

    client
        .set_cursor(window, CursorShape::Text)
        .expect("cursor");
    client
        .set_tooltip(window, sample_menu_anchor(), tip("Copy"))
        .expect("tooltip");
    client.set_title(window, "b").expect("title");
    client
        .set_sizing(window, WindowSizing::Fixed)
        .expect("sizing");
    client.resize(window, 20, 2, &BIG).expect("resize");
    {
        let host = &loopback.borrow().host;
        assert!(host.cursors.is_empty() && host.tooltips.is_empty());
        assert!(host.retitled.is_empty() && host.resized_range.is_empty());
        assert!(host.resized.is_empty(), "nothing was served yet");
    }
    assert_eq!(client.take_owed(), None, "nothing is answered yet");

    loopback.borrow_mut().serve_posted();
    assert_eq!(client.take_owed(), None, "every answer was an acceptance");
    let host = &loopback.borrow().host;
    assert_eq!(host.cursors, [(window, CursorShape::Text)]);
    assert_eq!(host.retitled.len(), 1);
    assert_eq!(host.resized.len(), 1);
}

/// A post the endpoint refuses for room — this process's own calls fill its
/// place — waits rather than failing, and goes, in the order it was asked,
/// once one of the process's answers is reaped.
#[test]
fn a_post_refused_for_room_waits_for_an_answer_then_goes_in_order() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    {
        let mut inner = loopback.borrow_mut();
        inner.holds_posts = true;
        inner.lane = Some(1);
    }
    let sent_before = loopback.borrow().sent.len();
    client
        .set_cursor(window, CursorShape::Text)
        .expect("posted");
    client
        .set_title(window, "b")
        .expect("held for room, not refused");
    client
        .set_sizing(window, WindowSizing::Fixed)
        .expect("held for room, not refused");
    assert_eq!(
        loopback.borrow().sent.len(),
        sent_before + 1,
        "only the first reached the endpoint"
    );
    assert_eq!(client.take_owed(), None, "waiting for room is no refusal");

    for posted in [sent_before + 2, sent_before + 3, sent_before + 3] {
        loopback.borrow_mut().serve_posted();
        client.reap_posted();
        assert_eq!(
            loopback.borrow().sent.len(),
            posted,
            "each answer makes room for one, the oldest waiting"
        );
    }
    let host = &loopback.borrow().host;
    assert_eq!(host.cursors, [(window, CursorShape::Text)]);
    assert_eq!(host.retitled.len(), 1);
    assert_eq!(host.resized_range.len(), 1);
}

/// A resize refused for room makes no region and is delivered again once one
/// of the process's answers is reaped, so the window still ends at its size.
#[test]
fn a_resize_refused_for_room_is_deferred_and_delivered_again() {
    const BIG: DisplayMode = DisplayMode {
        width_px: 8,
        height_px: 3,
        stride_bytes: 32,
        format: DisplayFormat::Bgra8888,
    };
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (20, 2 * 96)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let resized = WindowEvent::Resized {
        window_id: window,
        width_px: 8,
        height_px: 3,
        state: WindowSizeState::Restored,
    };
    let mut events = WindowEvents::new(QueueSource::new([resized.to_le_bytes()]));
    assert_eq!(events.try_wait(&mut client), Ok(Some(resized)));
    {
        let mut inner = loopback.borrow_mut();
        inner.holds_posts = true;
        inner.lane = Some(1);
    }
    client
        .set_cursor(window, CursorShape::Text)
        .expect("fills the lane");
    assert_eq!(
        client.resize(window, 20, 2, &BIG),
        Err(Errno::WouldBlock),
        "refused for room"
    );
    assert!(!client.resize_in_flight(window), "nothing was sent");
    assert_eq!(
        events.try_wait(&mut client),
        Ok(None),
        "nothing answered yet"
    );

    loopback.borrow_mut().serve_posted();
    assert_eq!(
        events.try_wait(&mut client),
        Ok(Some(resized)),
        "the size is delivered again once room is made"
    );
    assert_eq!(events.try_wait(&mut client), Ok(None), "and only once");
}

/// A posted resize reaches the session before the present of the frame
/// painted at its size, which the app makes at once.
#[test]
fn a_posted_resize_is_served_before_the_present_that_follows_it() {
    const BIG: DisplayMode = DisplayMode {
        width_px: 8,
        height_px: 3,
        stride_bytes: 32,
        format: DisplayFormat::Bgra8888,
    };
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (20, 2 * 96)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    loopback.borrow_mut().holds_posts = true;

    client.resize(window, 20, 2, &BIG).expect("resize");
    client
        .present(window, 1, one(DamageRect::full(&BIG)))
        .expect("the present at the new size is served after the resize");
    assert_eq!(client.take_owed(), None, "the resize was accepted");
}

/// A burst of one kind keeps one request in flight and ends on the newest:
/// the ones in between are never sent.
#[test]
fn a_burst_of_one_kind_keeps_one_in_flight_and_ends_on_the_newest() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    loopback.borrow_mut().holds_posts = true;
    let sent_before = loopback.borrow().sent.len();

    for shape in [CursorShape::Text, CursorShape::Busy, CursorShape::Crosshair] {
        client.set_cursor(window, shape).expect("cursor");
    }
    assert_eq!(
        loopback.borrow().sent.len(),
        sent_before + 1,
        "one in flight"
    );

    loopback.borrow_mut().serve_posted();
    client.reap_posted();
    assert_eq!(
        loopback.borrow().sent.len(),
        sent_before + 2,
        "the newest is posted once the first is answered"
    );
    loopback.borrow_mut().serve_posted();
    client.reap_posted();
    assert_eq!(
        loopback.borrow().host.cursors,
        [
            (window, CursorShape::Text),
            (window, CursorShape::Crosshair)
        ]
    );
    assert_eq!(client.take_owed(), None);
}

/// A resize asked while another is unanswered makes no region and is
/// delivered again once that one is answered, so the window ends at the
/// newest size the session reported.
#[test]
fn a_resize_asked_while_another_is_unanswered_is_delivered_again() {
    const BIG: DisplayMode = DisplayMode {
        width_px: 8,
        height_px: 3,
        stride_bytes: 32,
        format: DisplayFormat::Bgra8888,
    };
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (20, 2 * 96)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    loopback.borrow_mut().holds_posts = true;
    let resized = |width_px| WindowEvent::Resized {
        window_id: window,
        width_px,
        height_px: 3,
        state: WindowSizeState::Restored,
    };
    let mut events = WindowEvents::new(QueueSource::new([resized(8).to_le_bytes()]));
    assert_eq!(events.try_wait(&mut client), Ok(Some(resized(8))));
    client.resize(window, 20, 2, &BIG).expect("posted");
    let mut later = WindowEvents::new(QueueSource::new([resized(12).to_le_bytes()]));
    assert_eq!(later.try_wait(&mut client), Ok(Some(resized(12))));
    assert!(client.resize_in_flight(window), "the first is unanswered");
    assert_eq!(
        client.resize(window, 20, 2, &BIG),
        Err(Errno::WouldBlock),
        "no second resize is sent while the first is unanswered"
    );

    loopback.borrow_mut().serve_posted();
    assert_eq!(
        later.try_wait(&mut client),
        Ok(Some(resized(12))),
        "the newest size is delivered again"
    );
    assert!(!client.resize_in_flight(window));
}

/// Only a *consecutive* run folds, and only for the same window: the event
/// that ends the run is put back untouched, so folding can neither reorder
/// the queue nor lose anything from it.
#[test]
fn folding_a_resize_run_keeps_every_other_event_in_order() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");

    let resized = |window_id, width_px| WindowEvent::Resized {
        window_id,
        width_px,
        height_px: 50,
        state: WindowSizeState::Restored,
    };
    let key = WindowEvent::Focus {
        window_id: window,
        focused: true,
    };
    let mut waiter = WindowEvents::new(QueueSource::new([
        resized(window, 100).to_le_bytes(),
        resized(window, 120).to_le_bytes(),
        key.to_le_bytes(),
        resized(window, 140).to_le_bytes(),
        resized(window + 1, 160).to_le_bytes(),
    ]));
    assert_eq!(waiter.try_wait(&mut client), Ok(Some(resized(window, 120))));
    assert_eq!(waiter.try_wait(&mut client), Ok(Some(key)));
    assert_eq!(
        waiter.try_wait(&mut client),
        Ok(Some(resized(window, 140))),
        "a resize for another window does not join this one's run"
    );
    assert_eq!(
        waiter.try_wait(&mut client),
        Ok(Some(resized(window + 1, 160)))
    );
    assert_eq!(waiter.try_wait(&mut client), Ok(None));
}

/// The drained path answers a redraw request on the app's behalf exactly as
/// the parked one does, so a loop that interleaves work cannot leave a window
/// blank where a parked loop would have re-presented it.
#[test]
fn the_drained_wait_answers_a_redraw_request_like_the_parked_one() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");
    client
        .present(window, 1, full_damage())
        .expect("the first present");
    assert_eq!(loopback.borrow().host.presented.len(), 1);

    let mut waiter = WindowEvents::new(redraw_source(window));
    assert_eq!(
        waiter.try_wait(&mut client),
        Ok(Some(WindowEvent::RedrawRequested { window_id: window }))
    );
    let inner = loopback.borrow();
    assert_eq!(inner.host.presented.len(), 2);
    let (id, _, seen) = inner.host.presented.last().expect("the re-present");
    assert_eq!(*id, window);
    assert_eq!(*seen, full_damage());
}

/// A source holding one queued redraw request for `window`, ready for the
/// typed wait.
fn redraw_source(window: u64) -> QueueSource {
    QueueSource::new([WindowEvent::RedrawRequested { window_id: window }.to_le_bytes()])
}

#[test]
fn a_redraw_request_re_presents_the_last_frame_without_the_app_acting() {
    let loopback = Loopback::with_regions(&[(7, 2 * FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 2, "a").expect("a");

    // A client that has never presented has no frame to re-send: the
    // request is a no-op, not an error.
    let mut waiter = WindowEvents::new(redraw_source(window));
    assert_eq!(
        waiter.wait(&mut client),
        Ok(Some(WindowEvent::RedrawRequested { window_id: window }))
    );
    assert!(loopback.borrow().host.presented.is_empty());

    // After a present the library re-sends that frame with full-window
    // damage, so the app sees the event but need do nothing. The partial
    // damage of the original present is deliberately widened: a
    // re-established surface starts transparent, so only a full-window
    // present makes it correct again.
    let partial = DamageRect {
        x: 1,
        y: 1,
        width_px: 2,
        height_px: 1,
    };
    client.present(window, 1, one(partial)).expect("present");
    assert_eq!(loopback.borrow().host.presented.len(), 1);
    let mut waiter = WindowEvents::new(redraw_source(window));
    assert_eq!(
        waiter.wait(&mut client),
        Ok(Some(WindowEvent::RedrawRequested { window_id: window }))
    );
    {
        let inner = loopback.borrow();
        assert_eq!(inner.host.presented.len(), 2);
        let (id, _, seen) = inner.host.presented.last().expect("the re-present");
        assert_eq!(*id, window);
        assert_eq!(*seen, full_damage());
    }

    // A request naming a window this client does not own re-presents
    // nothing (fail closed).
    assert_eq!(client.answer_redraw(window + 500), Ok(false));
    assert_eq!(loopback.borrow().host.presented.len(), 2);
}

#[test]
fn event_routing_fails_closed() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");

    let mut sink = QueueSink::default();
    let inner = &mut *loopback.borrow_mut();
    // An unknown window routes nowhere.
    assert_eq!(
        inner
            .server
            .deliver_event(&mut sink, &WindowEvent::CloseRequested { window_id: 99 }),
        Err(Errno::NotFound)
    );
    // A pointer position outside the window's surface is a routing bug,
    // refused rather than delivered.
    assert_eq!(
        inner.server.deliver_event(
            &mut sink,
            &WindowEvent::Pointer {
                window_id: window,
                x: SURFACE.width_px,
                y: 0,
                action: PointerAction::Moved,
                modifiers: Modifiers::default(),
                at_ns: 0,
            }
        ),
        Err(Errno::OutOfRange)
    );
    // A pick concludes only through `conclude_pick`, which records what it
    // chose; delivered as a plain event it is refused — for both conclusions.
    assert_eq!(
        inner.server.deliver_event(
            &mut sink,
            &WindowEvent::FilePicked {
                window_id: window,
                handle: 7,
                writable: false,
            }
        ),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        inner
            .server
            .deliver_event(&mut sink, &WindowEvent::PickCancelled { window_id: window }),
        Err(Errno::OutOfRange)
    );
    // And one no request preceded is a session bug, refused rather than
    // delivered.
    assert_eq!(
        inner.server.conclude_pick(&mut sink, window, None),
        Err(Errno::OutOfRange)
    );
    assert!(sink.delivered.is_empty());
}

/// The catalog is the host's, paged into a bounded reply, and a caller
/// asking past its end gets an honest empty page rather than a refusal.
#[test]
fn the_wallpaper_catalog_is_answered_as_pages_of_the_hosts_own_listing() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    loopback.borrow_mut().host.wallpapers = alloc::vec![
        WallpaperName {
            category: String::from("TAIRiX"),
            file: String::from("a.jpg"),
        },
        WallpaperName {
            category: String::from("Space"),
            file: String::from("b.png"),
        },
    ];
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let mut page = [0u8; tairix_abi::window_ipc::WINDOW_WALLPAPERS_REPLY_MAX];

    let answered = client.wallpapers(0, &mut page).expect("a catalog page");
    assert_eq!(answered.total, 2);
    let entries: Vec<(String, String)> = answered
        .entries()
        .map(|entry| {
            (
                String::from_utf8_lossy(entry.category).into_owned(),
                String::from_utf8_lossy(entry.file).into_owned(),
            )
        })
        .collect();
    assert_eq!(
        entries,
        alloc::vec![
            (String::from("TAIRiX"), String::from("a.jpg")),
            (String::from("Space"), String::from("b.png")),
        ]
    );

    // Asking from the second entry answers only the remainder, and asking
    // past the end answers nothing rather than refusing.
    let answered = client.wallpapers(1, &mut page).expect("a catalog page");
    assert_eq!(answered.len(), 1);
    let answered = client.wallpapers(9, &mut page).expect("a catalog page");
    assert_eq!(answered.total, 2);
    assert!(answered.is_empty());
}

/// The choice space is the host's, answered whole rather than paged: it
/// fits one frame by construction, so a chooser learns every set in one
/// call.
#[test]
fn the_cursor_sets_are_answered_whole_from_the_hosts_own_listing() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    loopback.borrow_mut().host.cursor_sets = alloc::vec![
        CursorSetName(String::from("Standard")),
        CursorSetName(String::from("High Visibility")),
    ];
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let mut frame = [0u8; tairix_abi::window_ipc::WINDOW_CURSOR_SETS_REPLY_MAX];

    let answered = client.cursor_sets(&mut frame).expect("the choice space");
    assert_eq!(answered.len(), 2);
    let names: Vec<String> = answered
        .names()
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .collect();
    assert_eq!(
        names,
        alloc::vec![String::from("Standard"), String::from("High Visibility")]
    );
}

/// A desktop that listed no store offers no sets of its own, which is an
/// answer rather than a failure: the built-in set is the client's to add.
#[test]
fn a_host_with_no_cursor_store_answers_an_empty_choice_space() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let mut frame = [0u8; tairix_abi::window_ipc::WINDOW_CURSOR_SETS_REPLY_MAX];
    let answered = client.cursor_sets(&mut frame).expect("the choice space");
    assert!(answered.is_empty());
    assert_eq!(answered.names().count(), 0);
}

/// The application a reserved request is decided against is the one the
/// kernel attests for that very call, handed to the host verbatim.
#[test]
fn a_notify_source_query_reaches_the_host_with_the_attested_application() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let settings = AppIdentity::new("os.tairix.settings", PublisherId::from_raw([7; 32]))
        .expect("a well-formed identity");
    loopback.borrow_mut().identity.apps = alloc::vec![(TICKET_A, settings)];
    loopback.borrow_mut().host.notify_sources =
        alloc::vec![BundleId::new("os.tairix.netstack").expect("a bounded identity")];
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let mut frame = [0u8; tairix_abi::window_ipc::WINDOW_NOTIFY_SOURCES_REPLY_MAX];

    let answered = client.notify_sources(&mut frame).expect("the host answers");
    assert!(answered.names().eq([b"os.tairix.netstack".as_slice()]));
    assert_eq!(loopback.borrow().host.asked_by, alloc::vec![Some(settings)]);

    assert_eq!(client.lock_screen(), Ok(()));
    assert_eq!(loopback.borrow().host.locks, 1);
}

/// The handle the recording host mints for a zone-store delegation.
const ZONE_GRANT: u64 = 41;

/// The zone store is delegated to the process the kernel attests is asking,
/// never one the request names, and the minted handle reaches it whole.
#[test]
fn a_zone_store_request_is_delegated_to_the_attested_caller() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let settings = AppIdentity::new("os.tairix.settings", PublisherId::from_raw([7; 32]))
        .expect("a well-formed identity");
    loopback.borrow_mut().identity.apps = alloc::vec![(TICKET_A, settings)];
    let mut client = WindowClient::new(Rc::clone(&loopback));

    assert_eq!(client.zone_store(), Ok(ZONE_GRANT));
    assert_eq!(
        loopback.borrow().host.zone_grants,
        alloc::vec![proc_id(0xA1)]
    );
    assert_eq!(loopback.borrow().host.asked_by, alloc::vec![Some(settings)]);
}

/// A host holding no store refuses with the minted-id frame's own shape, so
/// the client reads the refusal rather than a malformed reply.
#[test]
fn a_host_with_no_zone_store_refuses_it_whole() {
    let mapper = MockMapper::with_regions(&[(7, FRAME_LEN)]);
    let mut server = WindowServer::new(mapper, SERVER, CLIENT_FRAME_MAX);
    let mut host = MinimalHost;
    let mut identity = MockIdentity::holding_layer(&[]);
    let mut reply = [0u8; WINDOW_REPLY_MAX];
    let request = request_frame(&WindowRequest::TakeZoneStore);
    let outcome = server.serve(
        &mut host,
        &mut QueueSink::default(),
        &mut identity,
        TICKET_A,
        &request,
        &mut reply,
    );
    let len = replied(outcome);
    assert_eq!(
        tairix_abi::window_ipc::decode_minted_id_reply(&reply[..len]),
        Err(Errno::NotSupported)
    );
}

/// A caller running no verified bundle is handed to the host as exactly that,
/// and the host's refusal reaches the caller whole.
#[test]
fn a_caller_with_no_application_is_refused_by_the_host() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let mut frame = [0u8; tairix_abi::window_ipc::WINDOW_NOTIFY_SOURCES_REPLY_MAX];
    assert_eq!(
        client.notify_sources(&mut frame).map(|list| list.len()),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(client.lock_screen(), Err(Errno::PermissionDenied));
    assert_eq!(client.zone_store(), Err(Errno::PermissionDenied));
    assert_eq!(
        loopback.borrow().host.asked_by,
        alloc::vec![None, None, None]
    );
    assert_eq!(loopback.borrow().host.locks, 0);
    assert!(loopback.borrow().host.zone_grants.is_empty());
}

/// The desktop's Settings application, as the kernel attests it in these
/// tests.
fn settings_app() -> AppIdentity {
    AppIdentity::new("os.tairix.settings", PublisherId::from_raw([7; 32]))
        .expect("a well-formed identity")
}

/// A Settings window on `loopback`, attested as the desktop's Settings.
fn settings_window(
    loopback: &Rc<RefCell<Loopback>>,
    client: &mut WindowClient<Rc<RefCell<Loopback>>>,
) -> u64 {
    loopback.borrow_mut().identity.apps = alloc::vec![(TICKET_A, settings_app())];
    create_id(client, 7, EVENTS_A, 1, "Settings").expect("a window")
}

/// The configure task these tests ask for.
fn configure_task() -> SystemTask {
    SystemTask::Configure(
        tairix_abi::elevate::ElevateWords::new(&["cache.all", "off"]).expect("within bounds"),
    )
}

/// The authorisation outcomes the sink carried, in order.
fn authorised(sink: &QueueSink) -> Vec<AuthorisedOutcome> {
    sink.delivered
        .iter()
        .filter_map(|(_, frame)| match WindowEvent::from_bytes(frame) {
            Ok(WindowEvent::Authorised { outcome, .. }) => Some(outcome),
            _ => None,
        })
        .collect()
}

/// An authorisation reaches the host with the attested application, the
/// window and the task whole, and concludes exactly once.
#[test]
fn an_authorisation_reaches_the_host_and_concludes_once() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = settings_window(&loopback, &mut client);
    let mut sink = QueueSink::default();

    assert_eq!(client.authorise(Some(window), &configure_task()), Ok(()));
    assert_eq!(
        loopback.borrow().host.authorisations,
        alloc::vec![(
            Some(settings_app()),
            AuthoriseFor::Window(window),
            configure_task()
        )]
    );
    assert_eq!(
        client.authorise(Some(window), &SystemTask::ShowAccounts),
        Err(Errno::AlreadyExists),
        "one pends per window"
    );
    let conclude = |sink: &mut QueueSink, outcome| {
        loopback.borrow_mut().server.conclude_authorisation(
            sink,
            AuthoriseFor::Window(window),
            outcome,
            Vec::new(),
        )
    };
    assert_eq!(
        conclude(&mut sink, AuthorisedOutcome::Ran { exit_code: 0 }),
        Ok(())
    );
    assert_eq!(
        conclude(&mut sink, AuthorisedOutcome::Declined),
        Err(Errno::OutOfRange),
        "and it concluded that one"
    );
    assert_eq!(
        authorised(&sink),
        alloc::vec![AuthorisedOutcome::Ran { exit_code: 0 }]
    );
    assert_eq!(
        client.authorise(Some(window), &SystemTask::SetClock),
        Ok(()),
        "a fresh one is accepted once the last concluded"
    );
}

/// An application with no window asks from its icon-bar slot: only one it
/// declared, one at a time, answered on its bar route with no window named,
/// with what a captured task printed its to take once, and nothing left of
/// it once the application exits.
#[test]
fn a_slot_authorisation_is_answered_on_the_bar_route() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    loopback.borrow_mut().identity.apps = alloc::vec![(TICKET_A, settings_app())];
    let mut sink = QueueSink::default();
    let task = SystemTask::ShowAccounts;

    assert_eq!(
        client.authorise(None, &task),
        Err(Errno::NotFound),
        "no slot was declared"
    );
    assert!(loopback.borrow().host.authorisations.is_empty());
    client
        .set_app_bar(&sample_app_bar(EVENTS_B))
        .expect("declared");
    assert_eq!(client.authorise(None, &task), Ok(()));
    let slot = AuthoriseFor::Slot(proc_id(0xA1));
    assert_eq!(
        loopback.borrow().host.authorisations,
        alloc::vec![(Some(settings_app()), slot, task)]
    );
    assert_eq!(
        client.authorise(None, &task),
        Err(Errno::AlreadyExists),
        "one pends per slot"
    );
    let listing = b"alice 1000\n".to_vec();
    let captured = AuthorisedOutcome::Captured {
        exit_code: 0,
        len: u32::try_from(listing.len()).expect("short"),
    };
    let conclude = |sink: &mut QueueSink, asker, outcome, output: Vec<u8>| {
        loopback
            .borrow_mut()
            .server
            .conclude_authorisation(sink, asker, outcome, output)
    };
    assert_eq!(
        conclude(
            &mut sink,
            AuthoriseFor::Slot(proc_id(0xB2)),
            AuthorisedOutcome::Declined,
            Vec::new()
        ),
        Err(Errno::NotFound),
        "a slot no one declared"
    );
    assert_eq!(conclude(&mut sink, slot, captured, listing.clone()), Ok(()));
    assert_eq!(
        conclude(&mut sink, slot, AuthorisedOutcome::Declined, Vec::new()),
        Err(Errno::OutOfRange),
        "it concluded once"
    );
    assert_eq!(
        sink.delivered,
        alloc::vec![(
            EVENTS_B,
            WindowEvent::Authorised {
                window_id: None,
                outcome: captured,
            }
            .to_le_bytes()
        )]
    );
    assert_eq!(client.take_authorised_output(None), Ok(listing));
    assert_eq!(
        client.take_authorised_output(None),
        Err(Errno::NotFound),
        "one-shot"
    );

    assert_eq!(client.authorise(None, &task), Ok(()));
    {
        let inner = &mut *loopback.borrow_mut();
        inner.server.client_exited(&mut inner.host, proc_id(0xA1));
    }
    assert_eq!(
        loopback.borrow_mut().server.conclude_authorisation(
            &mut sink,
            slot,
            AuthorisedOutcome::Declined,
            Vec::new()
        ),
        Err(Errno::NotFound),
        "nothing is owed to an application that has gone"
    );
}

/// A window numbered zero names no window: asking for one, or for what its
/// task printed, is refused before anything is sent, rather than read as the
/// icon-bar slot the wire spells that way.
///
/// Regression: `Some(0)` went out as the slot's zero and asked for the slot.
#[test]
fn window_zero_is_refused_rather_than_taken_for_the_slot() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    loopback.borrow_mut().identity.apps = alloc::vec![(TICKET_A, settings_app())];
    client
        .set_app_bar(&sample_app_bar(EVENTS_B))
        .expect("declared");

    assert_eq!(
        client.authorise(Some(0), &SystemTask::ShowAccounts),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        client.take_authorised_output(Some(0)),
        Err(Errno::OutOfRange)
    );
    assert!(
        loopback.borrow().host.authorisations.is_empty(),
        "no slot was asked for"
    );
}

/// A window the caller does not own answers as one that never existed, and a
/// refused prompt leaves nothing pending.
#[test]
fn an_authorisation_is_refused_whole_for_a_foreign_window_or_a_refused_prompt() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = settings_window(&loopback, &mut client);
    let mut sink = QueueSink::default();

    loopback.borrow_mut().ticket = TICKET_B;
    assert_eq!(
        client.authorise(Some(window), &configure_task()),
        Err(Errno::NotFound)
    );
    loopback.borrow_mut().ticket = TICKET_A;
    assert!(loopback.borrow().host.authorisations.is_empty());

    loopback.borrow_mut().host.refuse_authorise = Some(Errno::SeatBusy);
    assert_eq!(
        client.authorise(Some(window), &configure_task()),
        Err(Errno::SeatBusy)
    );
    assert_eq!(
        loopback.borrow_mut().server.conclude_authorisation(
            &mut sink,
            AuthoriseFor::Window(window),
            AuthorisedOutcome::Declined,
            Vec::new()
        ),
        Err(Errno::OutOfRange),
        "nothing was owed"
    );
    assert!(sink.delivered.is_empty());
}

/// What a captured task printed is the owner's to take once, exactly as the
/// outcome states it, and an outcome disagreeing with the output is refused.
#[test]
fn a_captured_output_is_the_owners_to_take_once() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = settings_window(&loopback, &mut client);
    let mut sink = QueueSink::default();
    let listing = b"alice 1000\n".to_vec();
    let captured = AuthorisedOutcome::Captured {
        exit_code: 0,
        len: u32::try_from(listing.len()).expect("short"),
    };

    assert_eq!(
        client.take_authorised_output(Some(window)),
        Err(Errno::NotFound),
        "nothing was captured"
    );
    client
        .authorise(Some(window), &SystemTask::ShowAccounts)
        .expect("accepted");
    let mut conclude = |outcome, output: Vec<u8>| {
        loopback.borrow_mut().server.conclude_authorisation(
            &mut sink,
            AuthoriseFor::Window(window),
            outcome,
            output,
        )
    };
    assert_eq!(
        conclude(captured, b"short".to_vec()),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        conclude(AuthorisedOutcome::Ran { exit_code: 0 }, listing.clone()),
        Err(Errno::OutOfRange)
    );
    assert_eq!(conclude(captured, listing.clone()), Ok(()));

    loopback.borrow_mut().ticket = TICKET_B;
    assert_eq!(
        client.take_authorised_output(Some(window)),
        Err(Errno::NotFound),
        "another client cannot read what this window was shown"
    );
    loopback.borrow_mut().ticket = TICKET_A;
    assert_eq!(client.take_authorised_output(Some(window)), Ok(listing));
    assert_eq!(
        client.take_authorised_output(Some(window)),
        Err(Errno::NotFound),
        "one-shot"
    );
}

/// An authorisation's outcome goes through its conclusion and never through
/// the general delivery path.
#[test]
fn an_authorised_event_is_not_delivered_as_any_other() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = settings_window(&loopback, &mut client);
    client
        .authorise(Some(window), &SystemTask::SetClock)
        .expect("accepted");
    let mut sink = QueueSink::default();
    assert_eq!(
        loopback.borrow_mut().server.deliver_event(
            &mut sink,
            &WindowEvent::Authorised {
                window_id: Some(window),
                outcome: AuthorisedOutcome::Launched,
            }
        ),
        Err(Errno::OutOfRange)
    );
    assert!(sink.delivered.is_empty());
}

/// A caller running no verified bundle is handed to the host as exactly that.
#[test]
fn an_authorisation_from_no_application_is_refused_by_the_host() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a window");
    assert_eq!(
        client.authorise(Some(window), &configure_task()),
        Err(Errno::PermissionDenied)
    );
    assert_eq!(
        loopback.borrow().host.authorisations,
        alloc::vec![(None, AuthoriseFor::Window(window), configure_task())]
    );
}

/// An attestation that fails refuses the request before the host is asked.
#[test]
fn a_failed_attestation_refuses_a_reserved_request_before_the_host() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    loopback.borrow_mut().identity.attest_error = Some(Errno::NotFound);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    assert_eq!(client.lock_screen(), Err(Errno::NotFound));
    assert!(loopback.borrow().host.asked_by.is_empty());
}

/// A desktop that listed no store answers an empty catalog, not an error:
/// offering no shipped pictures is a fact, not a failure.
#[test]
fn a_host_with_no_catalog_answers_an_empty_page() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let mut page = [0u8; tairix_abi::window_ipc::WINDOW_WALLPAPERS_REPLY_MAX];
    let answered = client.wallpapers(0, &mut page).expect("a catalog page");
    assert_eq!(answered.total, 0);
    assert!(answered.is_empty());
}

/// A frame region is mapped as the caller's own: naming a handle another
/// client granted finds nothing, whatever its number.
#[test]
fn a_window_maps_only_a_frame_region_its_own_client_granted() {
    let loopback = Loopback::with_mapper(MockMapper::granted(&[
        (7, FRAME_LEN, proc_id(0xA1)),
        (8, FRAME_LEN, proc_id(0xB2)),
    ]));
    let mut client = WindowClient::new(Rc::clone(&loopback));
    assert_eq!(
        create_id(&mut client, 8, EVENTS_A, 1, "stolen"),
        Err(Errno::NotFound)
    );
    create_id(&mut client, 7, EVENTS_A, 1, "own").expect("its own region maps");
}

/// The clipboard is asked only for the caller's own window, and with the
/// region named as the caller's.
#[test]
fn the_clipboard_is_asked_only_for_the_callers_own_window_and_region() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");

    loopback.borrow_mut().ticket = TICKET_B;
    assert_eq!(
        client.set_clipboard(window, 0x44, 3, ClipboardKind::Text),
        Err(Errno::NotFound)
    );
    assert_eq!(client.get_clipboard(window, 0x44), Err(Errno::NotFound));
    assert!(loopback.borrow().host.clipboard.is_empty());

    loopback.borrow_mut().ticket = TICKET_A;
    client
        .set_clipboard(window, 0x44, 3, ClipboardKind::Text)
        .expect("set passed on");
    client.get_clipboard(window, 0x45).expect("get passed on");
    let own = |handle| (window, ClientRegion::of(proc_id(0xA1), handle));
    assert_eq!(
        loopback.borrow().host.clipboard,
        alloc::vec![own(0x44), own(0x45)]
    );
}

/// Owner-bound as a pick is, and each render concluded exactly once by its
/// own event. How many may be pending is the host's to decide, so the engine
/// refuses only a picture already pending at that size.
#[test]
fn a_preview_render_is_owner_bound_and_each_is_concluded_once_by_its_own_event() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let wallpaper = PreviewSubject::Wallpaper(0);
    let starfield = PreviewSubject::Screensaver(2);

    loopback.borrow_mut().ticket = TICKET_B;
    assert_eq!(
        client.render_preview((window, 0x99), wallpaper, (144, 81)),
        Err(Errno::NotFound)
    );
    loopback.borrow_mut().ticket = TICKET_A;

    client
        .render_preview((window, 0x99), wallpaper, (144, 81))
        .expect("render accepted");
    client
        .render_preview((window, 0x9A), starfield, (144, 81))
        .expect("a second picture pending beside the first");
    assert_eq!(
        client.render_preview((window, 0x9B), wallpaper, (144, 81)),
        Err(Errno::AlreadyExists),
        "one picture was owed twice"
    );
    assert_eq!(
        loopback.borrow().host.renders,
        alloc::vec![
            (
                window,
                ClientRegion::of(proc_id(0xA1), 0x99),
                PreviewSize {
                    subject: wallpaper,
                    width: 144,
                    height: 81
                }
            ),
            (
                window,
                ClientRegion::of(proc_id(0xA1), 0x9A),
                PreviewSize {
                    subject: starfield,
                    width: 144,
                    height: 81
                }
            ),
        ],
        "each region is named as the caller's own"
    );

    let mut sink = QueueSink::default();
    let concluded = |subject| WindowEvent::PreviewRendered {
        window_id: window,
        subject,
        width: 144,
        height: 81,
        outcome: PreviewOutcome::Rendered,
    };
    assert_eq!(
        deliver(
            &loopback,
            &mut sink,
            &concluded(PreviewSubject::Wallpaper(5))
        ),
        Err(Errno::OutOfRange),
        "a conclusion of a render never accepted"
    );
    deliver(&loopback, &mut sink, &concluded(wallpaper)).expect("conclusion delivered");
    assert_eq!(
        deliver(&loopback, &mut sink, &concluded(wallpaper)),
        Err(Errno::OutOfRange),
        "exactly one conclusion per acceptance"
    );
    deliver(&loopback, &mut sink, &concluded(starfield)).expect("the other is still owed");
    assert_eq!(sink.delivered.len(), 2);
    client
        .render_preview((window, 0x99), wallpaper, (144, 81))
        .expect("a fresh render is accepted");
}

/// A conclusion is never shed from a full mailbox, so a client that stopped
/// draining while it kept asking grew the session's hold-back without bound;
/// no render is accepted while one waits for the window.
#[test]
fn no_render_is_accepted_while_a_conclusion_waits_undelivered() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let subject = PreviewSubject::Wallpaper(0);
    loopback.borrow_mut().sink.held_render = Some((EVENTS_A, window));
    assert_eq!(
        client.render_preview((window, 0x99), subject, (144, 81)),
        Err(Errno::LimitExceeded)
    );
    assert!(
        loopback.borrow().host.renders.is_empty(),
        "the host was not asked"
    );
    loopback.borrow_mut().sink.held_render = None;
    client
        .render_preview((window, 0x99), subject, (144, 81))
        .expect("accepted once the conclusion went out");
}

/// A refused render leaves nothing pending, so the caller may ask again.
#[test]
fn a_refused_render_leaves_no_pending_conclusion() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    loopback.borrow_mut().host.refuse_render = Some(Errno::LengthOutOfRange);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let subject = PreviewSubject::Wallpaper(0);
    assert_eq!(
        client.render_preview((window, 0x99), subject, (144, 81)),
        Err(Errno::LengthOutOfRange)
    );
    loopback.borrow_mut().host.refuse_render = None;
    client
        .render_preview((window, 0x99), subject, (144, 81))
        .expect("the window was left free to ask again");
}

/// A screensaver preview is decided against the attested application and
/// reaches the host with its document whole.
#[test]
fn a_screensaver_preview_reaches_the_host_with_the_caller_it_was_attested_as() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    assert_eq!(
        client.preview_screensaver("screensaver.saver = os.tairix.saver.clock\n"),
        Err(Errno::PermissionDenied),
        "a caller running no verified bundle is refused by the host"
    );
    let settings = AppIdentity::new("os.tairix.settings", PublisherId::from_raw([7; 32]))
        .expect("a well-formed identity");
    loopback.borrow_mut().identity.apps = alloc::vec![(TICKET_A, settings)];
    assert_eq!(
        client.preview_screensaver("screensaver.saver = os.tairix.saver.clock\n"),
        Ok(())
    );
    assert_eq!(
        loopback.borrow().host.previews,
        alloc::vec![String::from("screensaver.saver = os.tairix.saver.clock\n")]
    );
    assert_eq!(client.preview_screensaver(""), Err(Errno::LengthOutOfRange));
}

/// A screensaver's picture is asked for with the caller it was attested as,
/// since it says what the user installed; a shipped wallpaper's needs none.
#[test]
fn a_screensaver_picture_names_its_caller_and_a_wallpaper_none() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let settings = AppIdentity::new("os.tairix.settings", PublisherId::from_raw([7; 32]))
        .expect("a well-formed identity");
    loopback.borrow_mut().identity.apps = alloc::vec![(TICKET_A, settings)];
    client
        .render_preview((window, 0x99), PreviewSubject::Wallpaper(0), (144, 81))
        .expect("render accepted");
    client
        .render_preview((window, 0x9A), PreviewSubject::Screensaver(2), (144, 81))
        .expect("render accepted");
    assert_eq!(
        loopback.borrow().host.render_callers,
        alloc::vec![None, Some(settings)]
    );
}

#[test]
fn pick_file_is_owner_bound_single_pending_and_concluded_by_delivery() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");

    // A window the caller does not own answers exactly like one that
    // never existed.
    loopback.borrow_mut().ticket = TICKET_B;
    assert_eq!(client.pick_file(window, OPEN), Err(Errno::NotFound));
    loopback.borrow_mut().ticket = TICKET_A;

    // The owner's request reaches the host with its purpose and pends; a
    // second request while pending is refused without touching the host
    // again.
    client.pick_file(window, OPEN).expect("pick accepted");
    assert_eq!(loopback.borrow().host.picks, alloc::vec![(window, OPEN)]);
    assert_eq!(client.pick_file(window, OPEN), Err(Errno::AlreadyExists));
    assert_eq!(loopback.borrow().host.picks.len(), 1);

    // The conclusion delivers to the owner's endpoint and clears the
    // pending pick, so the app may ask again.
    let mut sink = QueueSink::default();
    conclude(&loopback, &mut sink, window, Some((9, "report.txt", false)))
        .expect("conclusion delivered");
    assert_eq!(sink.delivered.len(), 1);
    assert_eq!(sink.delivered[0].0, EVENTS_A);
    assert_eq!(
        WindowEvent::from_bytes(&sink.delivered[0].1),
        Ok(WindowEvent::FilePicked {
            window_id: window,
            handle: 9,
            writable: false,
        })
    );
    // Exactly one conclusion follows each acceptance: a second one is
    // refused until a new pick is accepted.
    assert_eq!(
        conclude(&loopback, &mut sink, window, None),
        Err(Errno::OutOfRange)
    );
    let save = PickPurpose::Save {
        suggested: DocumentName::new("Untitled.txt").expect("a valid name"),
        formats: SaveFormats::ANY,
    };
    client
        .pick_file(window, save)
        .expect("a fresh pick is accepted");
    assert_eq!(
        loopback.borrow().host.picks[1],
        (window, save),
        "the host is told what to choose"
    );
    conclude(&loopback, &mut sink, window, None).expect("the cancel conclusion delivers");
    assert_eq!(
        conclude(&loopback, &mut sink, window, None),
        Err(Errno::OutOfRange),
        "and it concluded that pick"
    );
}

#[test]
fn the_chosen_name_is_the_owners_to_take_once() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let mut sink = QueueSink::default();

    assert_eq!(
        client.take_picked_name(window),
        Err(Errno::NotFound),
        "nothing was picked"
    );
    client.pick_file(window, OPEN).expect("pick accepted");
    assert_eq!(
        client.take_picked_name(window),
        Err(Errno::NotFound),
        "nor has it concluded"
    );
    conclude(&loopback, &mut sink, window, Some((9, "report.txt", false))).expect("concluded");

    loopback.borrow_mut().ticket = TICKET_B;
    assert_eq!(
        client.take_picked_name(window),
        Err(Errno::NotFound),
        "another client cannot learn what this window chose"
    );
    loopback.borrow_mut().ticket = TICKET_A;
    assert_eq!(
        client.take_picked_name(window),
        Ok(String::from("report.txt"))
    );
    assert_eq!(
        client.take_picked_name(window),
        Err(Errno::NotFound),
        "one-shot"
    );

    // A new pick forgets an untaken name, and a cancelled one leaves none.
    client.pick_file(window, OPEN).expect("pick accepted");
    conclude(&loopback, &mut sink, window, Some((10, "first.txt", true))).expect("concluded");
    assert_eq!(
        sink.delivered
            .back()
            .map(|(_, event)| WindowEvent::from_bytes(event)),
        Some(Ok(WindowEvent::FilePicked {
            window_id: window,
            handle: 10,
            writable: true,
        })),
        "the owner learns it may write what it was handed"
    );
    client.pick_file(window, OPEN).expect("pick accepted");
    conclude(&loopback, &mut sink, window, None).expect("cancelled");
    assert_eq!(client.take_picked_name(window), Err(Errno::NotFound));
}

/// The files a folder pick delegated, each named and handled.
fn folder(files: &[(u64, &str)]) -> VecDeque<tairix_abi::window_ipc::PickedFile> {
    files
        .iter()
        .map(|&(handle, name)| tairix_abi::window_ipc::PickedFile {
            handle,
            name: DocumentName::new(name).expect("a valid name"),
        })
        .collect()
}

#[test]
fn a_folder_pick_concludes_with_its_files_for_the_owner_to_take_in_order() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let mut sink = QueueSink::default();

    client
        .pick_file(window, PickPurpose::Folder)
        .expect("pick accepted");
    assert_eq!(
        loopback.borrow().host.picks,
        alloc::vec![(window, PickPurpose::Folder)]
    );
    assert_eq!(
        conclude(&loopback, &mut sink, window, Some((9, "a.flac", false))),
        Err(Errno::OutOfRange),
        "a folder pick is not answered with one file"
    );
    let files = folder(&[(3, "01.flac"), (4, "02.flac")]);
    loopback
        .borrow_mut()
        .server
        .conclude_folder_pick(&mut sink, window, files, 5)
        .expect("concluded");
    assert_eq!(
        sink.delivered
            .back()
            .map(|(_, event)| WindowEvent::from_bytes(event)),
        Some(Ok(WindowEvent::FolderPicked {
            window_id: window,
            files: 2,
            left_out: 5
        }))
    );

    loopback.borrow_mut().ticket = TICKET_B;
    assert_eq!(
        client.take_picked_file(window).map(|file| file.handle),
        Err(Errno::NotFound),
        "another client cannot take what this window was handed"
    );
    loopback.borrow_mut().ticket = TICKET_A;
    assert_eq!(
        client.pick_file(window, OPEN),
        Err(Errno::AlreadyExists),
        "a further pick waits until the delegated files are taken"
    );
    let first = client.take_picked_file(window).expect("the first file");
    assert_eq!((first.handle, first.name.as_str()), (3, "01.flac"));
    let second = client.take_picked_file(window).expect("the second file");
    assert_eq!((second.handle, second.name.as_str()), (4, "02.flac"));
    assert_eq!(
        client.take_picked_file(window).map(|file| file.handle),
        Err(Errno::NotFound)
    );
    client
        .pick_file(window, PickPurpose::Folder)
        .expect("taken, a new pick is accepted");
    conclude(&loopback, &mut sink, window, None).expect("a folder pick may be cancelled");
}

#[test]
fn a_folder_conclusion_is_refused_unless_a_folder_was_asked_for_and_is_honest() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let mut sink = QueueSink::default();
    let server = |sink: &mut QueueSink, files, left_out| {
        loopback
            .borrow_mut()
            .server
            .conclude_folder_pick(sink, window, files, left_out)
    };
    assert_eq!(
        server(&mut sink, folder(&[(3, "a.flac")]), 0),
        Err(Errno::OutOfRange)
    );
    client.pick_file(window, OPEN).expect("pick accepted");
    assert_eq!(
        server(&mut sink, folder(&[(3, "a.flac")]), 0),
        Err(Errno::OutOfRange),
        "a file pick is not answered with a folder"
    );
    conclude(&loopback, &mut sink, window, None).expect("cancelled");
    client
        .pick_file(window, PickPurpose::Folder)
        .expect("pick accepted");
    assert_eq!(
        server(&mut sink, folder(&[(0, "a.flac")]), 0),
        Err(Errno::OutOfRange),
        "the reserved handle delegates nothing"
    );
    let names: Vec<String> = (0..=WINDOW_FOLDER_PICK_MAX)
        .map(|at| format!("{at}.flac"))
        .collect();
    let over: Vec<(u64, &str)> = names
        .iter()
        .zip(1..)
        .map(|(name, handle)| (handle, name.as_str()))
        .collect();
    assert_eq!(server(&mut sink, folder(&over), 0), Err(Errno::OutOfRange));
    assert_eq!(
        server(&mut sink, folder(&over[1..]), 1),
        Ok(()),
        "the bound is met"
    );
}

/// An application has at most `PICK_MAX_OPEN` picks open: one past it is
/// refused before the host hears of it, the refusal is the host's to audit,
/// and a pick concluding makes room again.
#[test]
fn an_application_has_at_most_its_bound_of_picks_open() {
    let bound = tairix_abi::window_ipc::PICK_MAX_OPEN;
    let handles: Vec<u64> = (7..).take(bound + 1).collect();
    let regions: Vec<(u64, usize)> = handles.iter().map(|&handle| (handle, FRAME_LEN)).collect();
    let loopback = Loopback::with_regions(&regions);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let windows: Vec<u64> = handles
        .iter()
        .map(|&handle| create_id(&mut client, handle, EVENTS_A, 1, "a").expect("a window"))
        .collect();
    for &window in &windows[..bound] {
        client.pick_file(window, OPEN).expect("within the bound");
    }
    let past = windows[bound];
    assert_eq!(client.pick_file(past, OPEN), Err(Errno::LimitExceeded));
    assert_eq!(
        loopback.borrow().host.picks.len(),
        bound,
        "the host never heard of the refused pick"
    );
    assert_eq!(
        loopback.borrow().host.picks_refused,
        [(proc_id(0xA1), past)]
    );
    assert_eq!(
        client.pick_file(windows[0], OPEN),
        Err(Errno::AlreadyExists),
        "one pick per window"
    );
    let mut sink = QueueSink::default();
    conclude(&loopback, &mut sink, windows[0], None).expect("cancelled");
    client
        .pick_file(past, OPEN)
        .expect("a pick concluded makes room");
}

/// One of an application's picks may choose a folder, until the files it
/// delegated are taken; a file pick beside it is not held back.
#[test]
fn an_application_has_one_folder_pick_open_until_its_files_are_taken() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let first = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let second = create_id(&mut client, 8, EVENTS_A, 1, "b").expect("b");
    client
        .pick_file(first, PickPurpose::Folder)
        .expect("accepted");
    assert_eq!(
        client.pick_file(second, PickPurpose::Folder),
        Err(Errno::LimitExceeded)
    );
    client
        .pick_file(second, OPEN)
        .expect("a file pick beside it is accepted");
    let mut sink = QueueSink::default();
    loopback
        .borrow_mut()
        .server
        .conclude_folder_pick(&mut sink, first, folder(&[(3, "01.flac")]), 0)
        .expect("concluded");
    conclude(&loopback, &mut sink, second, None).expect("cancelled");
    assert_eq!(
        client.pick_file(second, PickPurpose::Folder),
        Err(Errno::LimitExceeded),
        "the files it delegated are not taken yet"
    );
    client.take_picked_file(first).expect("taken");
    client
        .pick_file(second, PickPurpose::Folder)
        .expect("taken, another folder may be chosen");
    assert_eq!(
        loopback.borrow().host.picks_refused,
        [(proc_id(0xA1), second), (proc_id(0xA1), second)]
    );
}

/// A pick takes the keyboard only where its application holds the front, as
/// an alert does: the user is working in one of its windows, or made a
/// gesture at it, which the pick spends.
#[test]
fn a_pick_takes_the_keyboard_only_when_its_application_holds_the_front() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN), (9, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let mine = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let other = create_id(&mut client, 9, EVENTS_A, 1, "c").expect("c");
    loopback.borrow_mut().ticket = TICKET_B;
    let theirs = create_id(&mut client, 8, EVENTS_B, 1, "b").expect("b");
    loopback.borrow_mut().ticket = TICKET_A;

    loopback.borrow_mut().host.focused = Some(mine);
    client.pick_file(mine, OPEN).expect("accepted");
    loopback.borrow_mut().host.focused = Some(theirs);
    client.pick_file(other, OPEN).expect("accepted");
    assert_eq!(loopback.borrow().host.pick_focus, [true, false]);

    let mut sink = QueueSink::default();
    conclude(&loopback, &mut sink, mine, None).expect("cancelled");
    conclude(&loopback, &mut sink, other, None).expect("cancelled");
    client
        .set_app_bar(&sample_app_bar(EVENTS_A))
        .expect("declared");
    loopback
        .borrow_mut()
        .server
        .deliver_app_event(
            &mut sink,
            proc_id(0xA1),
            &WindowEvent::AppBarDefault,
            Activation::Granted {
                focus: Some(theirs),
            },
        )
        .expect("delivered");
    client.pick_file(mine, OPEN).expect("accepted");
    client.pick_file(other, OPEN).expect("accepted");
    assert_eq!(
        loopback.borrow().host.pick_focus,
        [true, false, true, false],
        "the activation is spent by the pick it earned"
    );
}

/// One openable file named `notes.txt`.
fn one_file() -> DragItems {
    DragItems::new(
        DocumentName::new("notes.txt").expect("a valid name"),
        1,
        true,
        [tairix_abi::window_ipc::DragPicture {
            icon: tairix_abi::window_ipc::IconAsset::new("text").expect("a valid asset id"),
            from: None,
            own: None,
        }],
    )
    .expect("one openable file")
}

/// A drag is the owner's, one at a time, concluded once — and only a drop
/// on an application leaves a target, which the owner takes once.
#[test]
fn a_drag_is_owner_bound_single_pending_and_concluded_once() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let mut sink = QueueSink::default();
    let target = DropTarget {
        run_path: BundleRunPath::new("/System/Applications/textedit.app/Run")
            .expect("a valid path"),
        writes_documents: true,
    };

    loopback.borrow_mut().ticket = TICKET_B;
    assert_eq!(client.begin_drag(window, one_file()), Err(Errno::NotFound));
    loopback.borrow_mut().ticket = TICKET_A;
    loopback.borrow_mut().host.refuse_drag = Some(Errno::PermissionDenied);
    assert_eq!(
        client.begin_drag(window, one_file()),
        Err(Errno::PermissionDenied),
        "a gesture the host cannot carry records nothing"
    );
    loopback.borrow_mut().host.refuse_drag = None;
    client.begin_drag(window, one_file()).expect("accepted");
    assert_eq!(
        loopback.borrow().host.drags,
        [(window, String::from("notes.txt"))]
    );
    assert_eq!(
        client.begin_drag(window, one_file()),
        Err(Errno::AlreadyExists)
    );
    assert_eq!(
        client.take_drop_target(window),
        Err(Errno::NotFound),
        "not dropped yet"
    );

    // Only its own conclusion ends it, and exactly once.
    for event in [
        WindowEvent::DragEnded {
            window_id: window,
            site: DropSite::Application,
        },
        WindowEvent::DragOver {
            window_id: window,
            serial: 1,
            at: DragAt::Nowhere,
            shift: false,
        },
    ] {
        assert_eq!(
            loopback
                .borrow_mut()
                .server
                .deliver_event(&mut sink, &event),
            Err(Errno::OutOfRange)
        );
    }
    loopback
        .borrow_mut()
        .server
        .conclude_drag(
            &mut sink,
            window,
            DragConclusion::Application(alloc::boxed::Box::new(target)),
        )
        .expect("concluded");
    assert_eq!(
        WindowEvent::from_bytes(&sink.delivered[0].1),
        Ok(WindowEvent::DragEnded {
            window_id: window,
            site: DropSite::Application,
        })
    );
    assert_eq!(
        loopback
            .borrow_mut()
            .server
            .conclude_drag(&mut sink, window, DragConclusion::Nothing),
        Err(Errno::OutOfRange)
    );
    assert_eq!(client.take_drop_target(window), Ok(target));
    assert_eq!(
        client.take_drop_target(window),
        Err(Errno::NotFound),
        "one-shot"
    );

    // A drag dropped on nothing leaves nothing to take.
    client.begin_drag(window, one_file()).expect("accepted");
    loopback
        .borrow_mut()
        .server
        .conclude_drag(&mut sink, window, DragConclusion::Nothing)
        .expect("concluded");
    assert_eq!(client.take_drop_target(window), Err(Errno::NotFound));
}

/// A loopback client over the test engine.
type LoopClient = WindowClient<Rc<RefCell<Loopback>>>;

/// Three windows — the dragging owner's two and a stranger's — with a drag
/// carried from the first.
fn carried_drag() -> (Rc<RefCell<Loopback>>, LoopClient, [u64; 3]) {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN), (9, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let source = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let sibling = create_id(&mut client, 8, EVENTS_A, 1, "b").expect("b");
    loopback.borrow_mut().ticket = TICKET_B;
    let stranger = create_id(&mut client, 9, EVENTS_B, 1, "c").expect("c");
    loopback.borrow_mut().ticket = TICKET_A;
    client.begin_drag(source, one_file()).expect("accepted");
    (loopback, client, [source, sibling, stranger])
}

/// Where a drag goes is told only to its owner and only about the owner's
/// own windows; a desktop folder is held for the report that named it.
#[test]
fn a_drag_is_reported_to_its_owner_about_its_own_windows_alone() {
    let (loopback, mut client, [source, sibling, stranger]) = carried_drag();
    let mut sink = QueueSink::default();
    let report = |sink: &mut QueueSink, serial, report| {
        loopback
            .borrow_mut()
            .server
            .report_drag(sink, source, serial, report, true)
    };
    assert_eq!(
        loopback
            .borrow_mut()
            .server
            .report_drag(&mut sink, sibling, 1, DragReport::Nowhere, false),
        Err(Errno::NotFound),
        "no drag is carried from that window"
    );

    report(
        &mut sink,
        1,
        DragReport::Window {
            window_id: sibling,
            x: 0,
            y: 0,
        },
    )
    .expect("a sibling window");
    assert_eq!(
        sink.delivered.pop_front(),
        Some((
            EVENTS_A,
            WindowEvent::DragOver {
                window_id: sibling,
                serial: 1,
                at: DragAt::Window { x: 0, y: 0 },
                shift: true,
            }
            .to_le_bytes()
        )),
        "addressed to the window under the pointer"
    );
    assert_eq!(
        report(
            &mut sink,
            2,
            DragReport::Window {
                window_id: stranger,
                x: 0,
                y: 0
            }
        ),
        Err(Errno::PermissionDenied),
        "another owner's window is never described"
    );
    assert_eq!(
        report(
            &mut sink,
            2,
            DragReport::Window {
                window_id: sibling,
                x: u32::MAX,
                y: 0
            }
        ),
        Err(Errno::OutOfRange)
    );

    report(
        &mut sink,
        3,
        DragReport::Desktop {
            folder: "/Users/ann/Desktop",
        },
    )
    .expect("the desktop");
    assert_eq!(
        client.drag_spot(source, 3).as_deref(),
        Ok("/Users/ann/Desktop")
    );
    assert_eq!(
        client.drag_spot(source, 2),
        Err(Errno::NotFound),
        "another report"
    );
    loopback.borrow_mut().ticket = TICKET_B;
    assert_eq!(
        client.drag_spot(source, 3),
        Err(Errno::NotFound),
        "only the owner reads it"
    );
}

/// Every report a pointer resting on one desktop folder makes names that
/// folder, so a report a later one superseded there is still read; a report
/// anywhere else ends the place.
#[test]
fn a_desktop_folder_is_held_for_every_report_of_its_place() {
    let (loopback, mut client, [source, sibling, _]) = carried_drag();
    let mut sink = QueueSink::default();
    let mut report = |serial, report| {
        loopback
            .borrow_mut()
            .server
            .report_drag(&mut sink, source, serial, report, false)
            .expect("reported");
    };
    let desktop = DragReport::Desktop { folder: "/D" };
    report(3, desktop);
    report(4, desktop);
    assert_eq!(
        client.drag_spot(source, 3).as_deref(),
        Ok("/D"),
        "a report superseded where the pointer rests"
    );
    assert_eq!(client.drag_spot(source, 4).as_deref(), Ok("/D"));
    report(
        5,
        DragReport::Window {
            window_id: sibling,
            x: 0,
            y: 0,
        },
    );
    report(6, desktop);
    assert_eq!(
        client.drag_spot(source, 4),
        Err(Errno::NotFound),
        "the pointer left that place"
    );
    assert_eq!(client.drag_spot(source, 6).as_deref(), Ok("/D"));
    report(7, DragReport::Desktop { folder: "/D/Inbox" });
    assert_eq!(
        client.drag_spot(source, 6),
        Err(Errno::NotFound),
        "another folder is another place"
    );
    assert_eq!(client.drag_spot(source, 7).as_deref(), Ok("/D/Inbox"));
}

/// A drag of three items led by `notes.txt`.
fn three_items() -> DragItems {
    let picture = tairix_abi::window_ipc::DragPicture {
        icon: tairix_abi::window_ipc::IconAsset::new("text").expect("a valid asset id"),
        from: None,
        own: None,
    };
    DragItems::new(
        DocumentName::new("notes.txt").expect("a valid name"),
        3,
        false,
        [picture; 3],
    )
    .expect("three items")
}

/// Item `item` about to be written under `name`.
fn making(item: u32, name: &str) -> LandingStep {
    LandingStep::Making {
        item,
        name: LandingName::new(name).expect("a valid name"),
    }
}

/// Item `item` made.
const fn made(item: u32) -> LandingStep {
    LandingStep::Made {
        item,
        outcome: LandingOutcome::Created,
    }
}

/// Drop window `window`'s carried drag on the desktop folder `/D` at report
/// `serial`, answering the landing that displaced.
fn drop_on_desktop(loopback: &Rc<RefCell<Loopback>>, window: u64, serial: u32) -> Option<u32> {
    let mut sink = QueueSink::default();
    let mut held = loopback.borrow_mut();
    let desktop = DragReport::Desktop { folder: "/D" };
    held.server
        .report_drag(&mut sink, window, serial, desktop, false)
        .expect("the desktop");
    let ended = DragConclusion::Desktop {
        serial,
        operation: DropOperation::Copy,
    };
    held.server
        .conclude_drag(&mut sink, window, ended)
        .expect("dropped")
}

/// A drop onto the desktop lands as its owner steps it, in drag order, an item
/// at a time, until the step that ends it; every other step is refused, and
/// none reaches the host.
#[test]
fn a_landing_is_stepped_by_its_owner_in_turn_until_it_ends() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    client.begin_drag(window, three_items()).expect("accepted");
    assert_eq!(drop_on_desktop(&loopback, window, 3), None);
    let mut transport = Rc::clone(&loopback);
    let mut step = |serial, step| crate::client::step_landing(&mut transport, window, serial, step);
    assert_eq!(
        step(4, LandingStep::Ended),
        Err(Errno::NotFound),
        "no drop landed at that report"
    );
    assert_eq!(
        step(3, made(0)),
        Err(Errno::OutOfRange),
        "nothing is being made"
    );
    assert_eq!(
        step(3, making(3, "d.txt")),
        Err(Errno::OutOfRange),
        "past the items dragged"
    );
    for refused in ["a/b", "..", "."] {
        assert_eq!(
            step(3, making(0, refused)),
            Err(Errno::OutOfRange),
            "no file name: {refused}"
        );
    }
    step(3, making(1, "b.txt")).expect("the second item");
    assert_eq!(
        step(3, making(2, "c.txt")),
        Err(Errno::OutOfRange),
        "one item at a time"
    );
    assert_eq!(
        step(3, made(2)),
        Err(Errno::OutOfRange),
        "only the item being made"
    );
    step(3, made(1)).expect("made");
    assert_eq!(
        step(3, making(0, "a.txt")),
        Err(Errno::OutOfRange),
        "in drag order"
    );
    step(3, making(2, "c.txt")).expect("the third item");
    loopback.borrow_mut().ticket = TICKET_B;
    assert_eq!(
        step(3, LandingStep::Ended),
        Err(Errno::NotFound),
        "only the owner steps it"
    );
    loopback.borrow_mut().ticket = TICKET_A;
    step(3, LandingStep::Ended).expect("ended");
    assert_eq!(
        step(3, LandingStep::Ended),
        Err(Errno::NotFound),
        "nothing is taken after its end"
    );
    assert_eq!(
        loopback.borrow().host.landings,
        [
            (window, 3, making(1, "b.txt")),
            (window, 3, made(1)),
            (window, 3, making(2, "c.txt")),
            (window, 3, LandingStep::Ended),
        ]
    );
}

/// A window lands at most `DROP_LANDINGS_MAX` drops, a fifth ending the
/// oldest; closing the window, or its owner going, ends every drop it was
/// landing.
#[test]
fn a_landing_ends_at_the_bound_and_with_its_window_or_owner() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let mut displaced = Vec::new();
    for serial in 1..=5 {
        client.begin_drag(window, one_file()).expect("accepted");
        displaced.push(drop_on_desktop(&loopback, window, serial));
    }
    assert_eq!(DROP_LANDINGS_MAX, 4);
    assert_eq!(displaced, [None, None, None, None, Some(1)]);
    let mut transport = Rc::clone(&loopback);
    assert_eq!(
        crate::client::step_landing(&mut transport, window, 1, LandingStep::Ended),
        Err(Errno::NotFound),
        "the fifth ended it"
    );
    client.close(window).expect("closed");
    let ended = |window, serial| (window, serial, LandingStep::Ended);
    assert_eq!(
        loopback.borrow().host.landings,
        [
            ended(window, 2),
            ended(window, 3),
            ended(window, 4),
            ended(window, 5)
        ]
    );

    let other = create_id(&mut client, 8, EVENTS_A, 1, "b").expect("b");
    client.begin_drag(other, one_file()).expect("accepted");
    assert_eq!(drop_on_desktop(&loopback, other, 6), None);
    {
        let inner = &mut *loopback.borrow_mut();
        inner.host.landings.clear();
        inner.server.client_exited(&mut inner.host, proc_id(0xA1));
        assert_eq!(inner.host.landings, [ended(other, 6)], "its owner went");
    }
}

/// A drop's end is posted, and each is its own: two asked back to back both
/// reach the desktop, and one it already ended is not reported as refused.
#[test]
fn each_drops_end_is_posted_on_its_own() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    for serial in [1, 2] {
        client.begin_drag(window, one_file()).expect("accepted");
        assert_eq!(drop_on_desktop(&loopback, window, serial), None);
    }
    loopback.borrow_mut().holds_posts = true;
    client.end_landing(window, 1).expect("posted");
    client.end_landing(window, 2).expect("posted");
    client.end_landing(window, 7).expect("posted");
    loopback.borrow_mut().serve_posted();
    client.reap_posted();
    assert_eq!(
        loopback.borrow().host.landings,
        [
            (window, 1, LandingStep::Ended),
            (window, 2, LandingStep::Ended)
        ]
    );
    assert_eq!(
        refusal(&mut client),
        None,
        "an end the desktop had made already"
    );
}

/// Every item the desktop's drag numbered `drag` offers window `window_id`,
/// read as `client`.
fn read_offered(
    client: &mut WindowClient<Rc<RefCell<Loopback>>>,
    window_id: u64,
    drag: u32,
) -> Result<Vec<(String, tairix_abi::fs::FileId)>, Errno> {
    let mut read = Vec::new();
    let total = client.read_offered_items(window_id, drag, |item| {
        read.push((String::from(item.path), item.id));
        Ok(())
    })?;
    assert_eq!(
        usize::try_from(total),
        Ok(read.len()),
        "every item it carries"
    );
    Ok(read)
}

/// More items than one reply holds are read a page at a time, in order, each
/// with its node, and a refusal from the reader stops the read.
#[test]
fn offered_items_past_one_page_are_read_in_order() {
    let (loopback, mut client, [_, _, stranger]) = carried_drag();
    let wanted: Vec<(String, tairix_abi::fs::FileId)> = (0..400u64)
        .map(|node| {
            let path = alloc::format!("/Users/ada/Desktop/a-rather-long-file-name-{node:04}.txt");
            let id = tairix_abi::fs::FileId {
                volume: [9; 16],
                node,
            };
            (path, id)
        })
        .collect();
    loopback.borrow_mut().host.offered_items.extend(
        wanted
            .iter()
            .map(|(path, id)| (stranger, 2, path.clone(), *id)),
    );
    loopback.borrow_mut().ticket = TICKET_B;
    let sent = loopback.borrow().sent.len();
    assert_eq!(read_offered(&mut client, stranger, 2), Ok(wanted));
    let pages = loopback.borrow().sent.len() - sent;
    assert!(
        (2..400).contains(&pages),
        "a page a round trip, never an item: {pages}"
    );
    let mut seen = 0;
    assert_eq!(
        client.read_offered_items(stranger, 2, |_| {
            seen += 1;
            if seen == 3 {
                Err(Errno::OutOfMemory)
            } else {
                Ok(())
            }
        }),
        Err(Errno::OutOfMemory)
    );
    assert_eq!(seen, 3, "nothing after the refusal");
}

/// The desktop's items offer is answered by the offered window's owner alone,
/// and its items are read through the host by that owner alone; an offer or
/// a drop outside the window is never delivered.
#[test]
fn an_items_offer_is_answered_and_read_by_its_windows_owner_alone() {
    let (loopback, mut client, [source, _, stranger]) = carried_drag();
    let node = tairix_abi::fs::FileId {
        volume: [3; 16],
        node: 41,
    };
    loopback.borrow_mut().host.offered_items.push((
        stranger,
        7,
        String::from("/Users/ada/Desktop/notes.txt"),
        node,
    ));
    loopback.borrow_mut().ticket = TICKET_B;
    client
        .answer_items_offer(source, 3, Some(DropOperation::Copy))
        .expect("posted");
    assert_eq!(
        refusal(&mut client),
        Some(refused(
            source,
            PostedRequest::ItemsOfferAnswer,
            Errno::NotFound
        )),
        "only the owner answers"
    );
    assert_eq!(read_offered(&mut client, source, 7), Err(Errno::NotFound));
    client
        .answer_items_offer(stranger, 3, Some(DropOperation::Move))
        .expect("answered");
    assert_eq!(
        loopback.borrow().host.items_answers,
        [(stranger, 3, Some(DropOperation::Move))]
    );
    assert_eq!(
        read_offered(&mut client, stranger, 7),
        Ok(alloc::vec![(
            String::from("/Users/ada/Desktop/notes.txt"),
            node
        )])
    );
    assert_eq!(read_offered(&mut client, stranger, 8), Err(Errno::NotFound));

    let mut sink = QueueSink::default();
    let offer = |x| WindowEvent::ItemsOffered {
        window_id: stranger,
        drag: 7,
        serial: 5,
        x,
        y: 0,
        shift: false,
    };
    assert_eq!(
        loopback
            .borrow_mut()
            .server
            .deliver_event(&mut sink, &offer(u32::MAX)),
        Err(Errno::OutOfRange)
    );
    loopback
        .borrow_mut()
        .server
        .deliver_event(&mut sink, &offer(0))
        .expect("inside the window");
    let dropped = WindowEvent::ItemsOfferEnded {
        window_id: stranger,
        serial: 5,
        dropped: Some(tairix_abi::window_ipc::OfferDrop {
            x: 0,
            y: u32::MAX,
            operation: DropOperation::Copy,
        }),
    };
    assert_eq!(
        loopback
            .borrow_mut()
            .server
            .deliver_event(&mut sink, &dropped),
        Err(Errno::OutOfRange)
    );
}

/// A save offer is answered by the offered window's owner alone; a dropped
/// save's folder reaches the host with the attested caller whoever sent it,
/// so the host consumes every location; and an offer outside its window is
/// never delivered.
#[test]
fn a_save_offer_is_answered_by_its_windows_owner_and_its_folder_reaches_the_host() {
    let (loopback, mut client, [source, _, stranger]) = carried_drag();
    loopback.borrow_mut().ticket = TICKET_B;
    client.answer_save_offer(source, 3, true).expect("posted");
    assert_eq!(
        refusal(&mut client),
        Some(refused(
            source,
            PostedRequest::SaveOfferAnswer,
            Errno::NotFound
        )),
        "only the owner answers"
    );
    client
        .answer_save_offer(stranger, 3, true)
        .expect("answered");
    assert_eq!(loopback.borrow().host.save_answers, [(stranger, 3, true)]);
    client.save_into(source, 4, 41).expect("passed on");
    assert_eq!(
        loopback.borrow().host.save_folders,
        [(proc_id(0xB2), source, 4, 41)],
        "the host decides, knowing who sent it"
    );

    let mut sink = QueueSink::default();
    let offer = |x| WindowEvent::SaveOffered {
        window_id: stranger,
        serial: 5,
        x,
        y: 0,
    };
    assert_eq!(
        loopback
            .borrow_mut()
            .server
            .deliver_event(&mut sink, &offer(u32::MAX)),
        Err(Errno::OutOfRange)
    );
    loopback
        .borrow_mut()
        .server
        .deliver_event(&mut sink, &offer(0))
        .expect("inside the window");
    assert_eq!(WindowEvent::from_bytes(&sink.delivered[0].1), Ok(offer(0)));
    let ended = |dropped| WindowEvent::SaveOfferEnded {
        window_id: stranger,
        serial: 5,
        dropped,
    };
    assert_eq!(
        loopback
            .borrow_mut()
            .server
            .deliver_event(&mut sink, &ended(Some((0, u32::MAX)))),
        Err(Errno::OutOfRange),
        "a drop outside the window is never delivered"
    );
    for dropped in [None, Some((1, 1))] {
        loopback
            .borrow_mut()
            .server
            .deliver_event(&mut sink, &ended(dropped))
            .expect("left, or dropped inside");
    }
}

/// A drag is answered by its owner alone while it is carried, and a desktop
/// drop names a folder some report held, which outlives the drop.
#[test]
fn a_drag_is_answered_by_its_owner_and_dropped_where_a_report_named() {
    let (loopback, mut client, [source, _, stranger]) = carried_drag();
    let mut sink = QueueSink::default();
    loopback
        .borrow_mut()
        .server
        .report_drag(
            &mut sink,
            source,
            3,
            DragReport::Desktop { folder: "/D" },
            false,
        )
        .expect("the desktop");
    loopback.borrow_mut().ticket = TICKET_B;
    client
        .drag_verdict(source, 3, Some(DropOperation::Copy))
        .expect("posted");
    assert_eq!(
        refusal(&mut client),
        Some(refused(source, PostedRequest::DragVerdict, Errno::NotFound)),
        "only the owner answers"
    );
    loopback.borrow_mut().ticket = TICKET_A;
    client
        .drag_verdict(source, 3, Some(DropOperation::Move))
        .expect("answered");
    assert_eq!(
        loopback.borrow().host.verdicts,
        [(source, 3, Some(DropOperation::Move))]
    );

    let conclude = |sink: &mut QueueSink, ended| {
        loopback
            .borrow_mut()
            .server
            .conclude_drag(sink, source, ended)
    };
    let copy = DropOperation::Copy;
    assert_eq!(
        conclude(
            &mut sink,
            DragConclusion::Window {
                window_id: stranger,
                x: 0,
                y: 0,
                operation: copy
            }
        ),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        conclude(
            &mut sink,
            DragConclusion::Desktop {
                serial: 2,
                operation: copy
            }
        ),
        Err(Errno::OutOfRange),
        "no folder was held for that report"
    );
    sink.delivered.clear();
    conclude(
        &mut sink,
        DragConclusion::Desktop {
            serial: 3,
            operation: DropOperation::Move,
        },
    )
    .expect("dropped on the desktop");
    assert_eq!(
        WindowEvent::from_bytes(&sink.delivered[0].1),
        Ok(WindowEvent::DragEnded {
            window_id: source,
            site: DropSite::Desktop {
                serial: 3,
                operation: DropOperation::Move,
            },
        })
    );
    assert_eq!(
        client.drag_spot(source, 3).as_deref(),
        Ok("/D"),
        "read after the drop"
    );
    client.drag_verdict(source, 3, None).expect("posted");
    assert_eq!(
        refusal(&mut client),
        Some(refused(source, PostedRequest::DragVerdict, Errno::NotFound)),
        "not carried"
    );
    client.begin_drag(source, one_file()).expect("a new drag");
    assert_eq!(
        client.drag_spot(source, 3),
        Err(Errno::NotFound),
        "a new drag forgets it"
    );
}

/// A conclusion the sink refuses leaves the pick pending, so the window can
/// still be told later.
///
/// This is what makes a session's hold-back the fix rather than a
/// convenience: a sink that drops a refused conclusion would leave this
/// window's pick pending for the rest of its life — every later `pick_file`
/// refused `AlreadyExists`, with no conclusion that can ever clear it — while
/// a sink that *accepts* it (because it will deliver it from a hold-back)
/// concludes the pick exactly once, as the protocol says.
#[test]
fn a_conclusion_the_sink_refuses_stays_pending_until_one_is_accepted() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    client.pick_file(window, OPEN).expect("pick accepted");

    assert_eq!(
        conclude(
            &loopback,
            &mut FullSink,
            window,
            Some((9, "report.txt", false))
        ),
        Err(Errno::WouldBlock),
        "a full mailbox is relayed, not swallowed"
    );
    assert_eq!(
        client.pick_file(window, OPEN),
        Err(Errno::AlreadyExists),
        "the pick is still pending, so no second one starts"
    );
    assert_eq!(
        client.take_picked_name(window),
        Err(Errno::NotFound),
        "nor is a name held for a conclusion that never arrived"
    );

    // The session takes responsibility for the conclusion and the pick ends.
    let mut sink = QueueSink::default();
    conclude(&loopback, &mut sink, window, None).expect("an accepted conclusion concludes it");
    assert_eq!(sink.delivered.len(), 1);
    client
        .pick_file(window, OPEN)
        .expect("the window may pick again");
}

#[test]
fn a_refused_picker_leaves_no_pending_pick() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");

    // The host cannot run a picker (its one slot is taken, or it holds
    // no filesystem authority): the refusal is relayed verbatim and no
    // pick pends — a conclusion is still refused, and a later request
    // (once the host recovers) is accepted.
    loopback.borrow_mut().host.refuse_pick = Some(Errno::AlreadyExists);
    assert_eq!(client.pick_file(window, OPEN), Err(Errno::AlreadyExists));
    let mut sink = QueueSink::default();
    assert_eq!(
        conclude(&loopback, &mut sink, window, None),
        Err(Errno::OutOfRange)
    );
    loopback.borrow_mut().host.refuse_pick = None;
    client
        .pick_file(window, OPEN)
        .expect("accepted once the host can");
}

#[test]
fn set_backdrop_blur_is_owner_bound_and_reaches_the_host() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");

    // A window the caller does not own is refused, and the host is
    // never told.
    loopback.borrow_mut().ticket = TICKET_B;
    assert_eq!(client.set_backdrop_blur(window, 8), Err(Errno::NotFound));
    assert!(loopback.borrow().host.blur_sets.is_empty());
    loopback.borrow_mut().ticket = TICKET_A;

    // The owner's radius reaches the host exactly, and a later call
    // replaces it rather than accumulating.
    client.set_backdrop_blur(window, 12).expect("set");
    assert_eq!(loopback.borrow().host.blur_sets, alloc::vec![(window, 12)]);
    client.set_backdrop_blur(window, 0).expect("disabled");
    assert_eq!(
        loopback.borrow().host.blur_sets,
        alloc::vec![(window, 12), (window, 0)]
    );
}

#[test]
fn backdrop_blur_defaults_to_an_accepted_no_op() {
    // A host implementing only the mandatory bridge methods exercises the
    // trait's own default: a host with no compositor to tell accepts the
    // radius and draws nothing, rather than failing a request it validated.
    let mapper = MockMapper::with_regions(&[(7, FRAME_LEN)]);
    let mut server = WindowServer::new(mapper, SERVER, CLIENT_FRAME_MAX);
    let mut host = MinimalHost;
    let mut identity = MockIdentity::holding_layer(&[]);
    let mut reply = [0u8; WINDOW_REPLY_MAX];

    let create = request_frame(&WindowRequest::Create {
        shm_handle: 7,
        event_endpoint: EVENTS_A,
        frame_count: 1,
        width_px: SURFACE.width_px,
        height_px: SURFACE.height_px,
        stride_bytes: SURFACE.stride_bytes,
        format: SURFACE.format,
        title: tairix_abi::window_ipc::WindowTitle::new("a").expect("valid title"),
        sizing: WindowSizing::Fixed,
    });
    let outcome = server.serve(
        &mut host,
        &mut QueueSink::default(),
        &mut identity,
        TICKET_A,
        &create,
        &mut reply,
    );
    let len = replied(outcome);
    let (window, _) = tairix_abi::window_ipc::decode_create_reply(&reply[..len]).expect("created");

    // Setting the backdrop blur is infallible for an owned window: the
    // default handler has no compositor to tell.
    let blur = request_frame(&WindowRequest::SetBackdropBlur {
        window_id: window,
        radius_px: 8,
    });
    let outcome = server.serve(
        &mut host,
        &mut QueueSink::default(),
        &mut identity,
        TICKET_A,
        &blur,
        &mut reply,
    );
    let len = replied(outcome);
    assert_eq!(decode_status_reply(&reply[..len]), Ok(()));
}

/// The menu a test application opens for one of its windows: a titled root
/// plate with one chooseable row and one submenu row.
fn sample_open_menu() -> AppMenu {
    let mut menu = AppMenu::titled(AppMenuLabel::new("Edit").expect("a valid title"));
    menu.push(AppMenuRow::Item(AppMenuItem::new(
        AppMenuItemId::new(1).expect("a valid id"),
        AppMenuLabel::new("Copy").expect("a valid label"),
        AppMenuSymbol::Copy,
    )))
    .expect("room");
    menu.push(AppMenuRow::Submenu {
        label: AppMenuLabel::new("Paste Special").expect("a valid label"),
        enabled: true,
        symbol: AppMenuSymbol::Paste,
        reason: AppMenuReason::EMPTY,
    })
    .expect("room for a submenu row");
    menu
}

/// The anchor a right-click hands back: the window-local point the app was
/// given, with no extent.
fn sample_menu_anchor() -> WindowRegion {
    WindowRegion::new(12, 30, 0, 0).expect("a representable anchor")
}

/// One accepted open is answered exactly once, and its answer names the open
/// it belongs to.
///
/// The open id is what makes that a property rather than a hope: an
/// application that asked again while a previous answer was still in its
/// mailbox would otherwise read one gesture's dismissal as the next one's.
#[test]
fn a_menu_open_is_owner_bound_single_pending_and_answered_exactly_once() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let menu = sample_open_menu();
    let anchor = sample_menu_anchor();

    // A window the caller does not own answers exactly like one that never
    // existed, and the host is never told.
    loopback.borrow_mut().ticket = TICKET_B;
    assert_eq!(
        client.open_menu(window, anchor, &menu),
        Err(Errno::NotFound)
    );
    assert!(loopback.borrow().host.menu_opens.is_empty());
    loopback.borrow_mut().ticket = TICKET_A;

    // The owner's open reaches the host with the anchor and the whole menu
    // — title included — and mints an id.
    let open = client
        .open_menu(window, anchor, &menu)
        .expect("the open is accepted");
    assert_ne!(open, 0, "an open id is never zero");
    {
        let host = &loopback.borrow().host;
        assert_eq!(host.menu_opens.len(), 1);
        let (told_window, told_open, told_anchor, told_menu) = &host.menu_opens[0];
        assert_eq!((*told_window, *told_open), (window, open));
        assert_eq!(*told_anchor, anchor);
        assert_eq!(told_menu.title(), "Edit");
        assert_eq!(told_menu.len(), 2);
        assert!(told_menu.rows().any(|(row, _)| matches!(
            row,
            AppMenuRowView::Submenu { label, enabled, .. }
                if label == "Paste Special" && enabled
        )));
    }

    // While it is unanswered a second open is refused without troubling the
    // host: the chain holds the seat's grab, so a well-behaved application
    // never reaches here.
    assert_eq!(
        client.open_menu(window, anchor, &menu),
        Err(Errno::AlreadyExists)
    );
    assert_eq!(loopback.borrow().host.menu_opens.len(), 1);

    // An outcome for some other open answers nothing and is refused rather
    // than delivered.
    let mut sink = QueueSink::default();
    assert_eq!(
        deliver(
            &loopback,
            &mut sink,
            &WindowEvent::MenuClosed {
                window_id: window,
                open_id: open + 1,
                outcome: MenuOutcome::Dismissed,
            }
        ),
        Err(Errno::OutOfRange)
    );
    assert!(sink.delivered.is_empty());

    // The outcome that names it delivers to the owner's endpoint, once.
    let chosen = WindowEvent::MenuClosed {
        window_id: window,
        open_id: open,
        outcome: MenuOutcome::Chosen(AppMenuItemId::new(1).expect("a valid id")),
    };
    deliver(&loopback, &mut sink, &chosen).expect("the outcome delivers");
    assert_eq!(sink.delivered.len(), 1);
    assert_eq!(sink.delivered[0].0, EVENTS_A);
    assert_eq!(sink.delivered[0].1, chosen.to_le_bytes());

    // A second outcome for the same open — the same one replayed, or a
    // contradicting dismissal — cannot be delivered.
    for outcome in [
        MenuOutcome::Chosen(AppMenuItemId::new(1).expect("a valid id")),
        MenuOutcome::Dismissed,
    ] {
        assert_eq!(
            deliver(
                &loopback,
                &mut sink,
                &WindowEvent::MenuClosed {
                    window_id: window,
                    open_id: open,
                    outcome,
                }
            ),
            Err(Errno::OutOfRange),
            "exactly one outcome follows an acceptance"
        );
    }
    assert_eq!(sink.delivered.len(), 1);

    // Answered, the window may open again — under a fresh id, so the two
    // gestures' answers can never be confused.
    let again = client
        .open_menu(window, anchor, &menu)
        .expect("a fresh open is accepted");
    assert_ne!(again, open, "an open id is never reused");
}

/// The text the user committed into a chain's quick-entry field is held for
/// the owning window alone, answered once, and cleared by the next open.
///
/// The commit is pulled rather than delivered because an event is one fixed
/// frame and a name is wider than that, so the rules that make an outcome
/// unmistakable have to hold for the pull too.
#[test]
fn committed_menu_text_is_owner_bound_taken_once_and_cleared_by_the_next_open() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let other = create_id(&mut client, 8, EVENTS_A, 1, "b").expect("b");
    let menu = sample_open_menu();
    let anchor = sample_menu_anchor();
    let open = client
        .open_menu(window, anchor, &menu)
        .expect("the open is accepted");

    // With nothing recorded the pull answers the honest empty rather than a
    // fabricated name.
    assert_eq!(client.take_menu_text(window, open), Ok(None));

    // A text may only be recorded against the open that window is still
    // waiting on: any other names a gesture whose answer has gone.
    assert_eq!(
        loopback
            .borrow_mut()
            .server
            .record_menu_text(window, open + 1, "stale.txt"),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        loopback
            .borrow_mut()
            .server
            .record_menu_text(other, open, "elsewhere.txt"),
        Err(Errno::OutOfRange)
    );
    // And never wider than a field can hold.
    let over = "n".repeat(APP_MENU_ENTRY_MAX + 1);
    assert_eq!(
        loopback
            .borrow_mut()
            .server
            .record_menu_text(window, open, &over),
        Err(Errno::LengthOutOfRange)
    );

    loopback
        .borrow_mut()
        .server
        .record_menu_text(window, open, "report.txt")
        .expect("the commit is recorded against its own open");

    // A window the caller does not own answers exactly like one that never
    // existed, and reads nothing.
    loopback.borrow_mut().ticket = TICKET_B;
    assert_eq!(
        client.take_menu_text(window, open),
        Err(Errno::NotFound),
        "a foreign pull is refused"
    );
    loopback.borrow_mut().ticket = TICKET_A;

    // A pull naming another open answers nothing and leaves the held text
    // for the pull that names it.
    assert_eq!(client.take_menu_text(window, open + 1), Ok(None));
    assert_eq!(
        client.take_menu_text(window, open),
        Ok(Some(String::from("report.txt")))
    );
    // Taken once: two readers cannot both act on one commit.
    assert_eq!(client.take_menu_text(window, open), Ok(None));

    // A commit the application never pulled belongs to the gesture that is
    // over, so the next open on that window clears it.
    let mut sink = QueueSink::default();
    deliver(
        &loopback,
        &mut sink,
        &WindowEvent::MenuClosed {
            window_id: window,
            open_id: open,
            outcome: MenuOutcome::Entered(AppMenuItemId::new(90).expect("a valid id")),
        },
    )
    .expect("the outcome delivers");
    let again = client
        .open_menu(window, anchor, &menu)
        .expect("a fresh open is accepted");
    loopback
        .borrow_mut()
        .server
        .record_menu_text(window, again, "unpulled.txt")
        .expect("recorded against the fresh open");
    deliver(
        &loopback,
        &mut sink,
        &WindowEvent::MenuClosed {
            window_id: window,
            open_id: again,
            outcome: MenuOutcome::Dismissed,
        },
    )
    .expect("the outcome delivers");
    let third = client
        .open_menu(window, anchor, &menu)
        .expect("a third open is accepted");
    assert_eq!(
        client.take_menu_text(window, again),
        Ok(None),
        "the next open cleared the commit nobody pulled"
    );
    assert_eq!(client.take_menu_text(window, third), Ok(None));
}

/// Two windows each hold their own open, and each answer reaches only its
/// own — which is what lets the seat's singleton close one chain and answer
/// *it* while another window's open stands.
#[test]
fn each_windows_open_is_answered_on_its_own() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let first = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let second = create_id(&mut client, 8, EVENTS_A, 1, "b").expect("b");
    let menu = sample_open_menu();
    let anchor = sample_menu_anchor();

    let first_open = client
        .open_menu(first, anchor, &menu)
        .expect("the first open");
    let second_open = client
        .open_menu(second, anchor, &menu)
        .expect("the second open");
    assert_ne!(first_open, second_open);

    // The first window's open is answered by its own id and no other.
    let mut sink = QueueSink::default();
    assert_eq!(
        deliver(
            &loopback,
            &mut sink,
            &WindowEvent::MenuClosed {
                window_id: first,
                open_id: second_open,
                outcome: MenuOutcome::Dismissed,
            }
        ),
        Err(Errno::OutOfRange),
        "an open id belongs to the window that asked"
    );
    deliver(
        &loopback,
        &mut sink,
        &WindowEvent::MenuClosed {
            window_id: first,
            open_id: first_open,
            outcome: MenuOutcome::Dismissed,
        },
    )
    .expect("the displaced chain's own requester is answered");

    // The second window's open still stands and is answered in its turn.
    deliver(
        &loopback,
        &mut sink,
        &WindowEvent::MenuClosed {
            window_id: second,
            open_id: second_open,
            outcome: MenuOutcome::Chosen(AppMenuItemId::new(1).expect("a valid id")),
        },
    )
    .expect("and the standing chain answers separately");
    assert_eq!(sink.delivered.len(), 2);
}

/// A refused open records nothing, spends no id, and leaves no outcome owed.
#[test]
fn a_refused_menu_open_records_nothing_and_spends_no_id() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let menu = sample_open_menu();
    let anchor = sample_menu_anchor();

    // The host cannot bring a chain up (its seat is held by a lock screen,
    // it is tearing down): the refusal is relayed verbatim.
    loopback.borrow_mut().host.refuse_menu_open = Some(Errno::NotSupported);
    assert_eq!(
        client.open_menu(window, anchor, &menu),
        Err(Errno::NotSupported)
    );

    // No open pends, so no outcome can be delivered for one.
    let mut sink = QueueSink::default();
    assert_eq!(
        deliver(
            &loopback,
            &mut sink,
            &WindowEvent::MenuClosed {
                window_id: window,
                open_id: 1,
                outcome: MenuOutcome::Dismissed,
            }
        ),
        Err(Errno::OutOfRange)
    );

    // And the refusal spent no id: the first accepted open is still the
    // first one minted.
    loopback.borrow_mut().host.refuse_menu_open = None;
    assert_eq!(
        client.open_menu(window, anchor, &menu),
        Ok(1),
        "a refused open consumes nothing"
    );
}

/// An outcome the sink refuses leaves the open owed, so the window can still
/// be told later — and cannot start a second chain in the meantime.
#[test]
fn an_outcome_the_sink_refuses_stays_owed_until_one_is_accepted() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let menu = sample_open_menu();
    let anchor = sample_menu_anchor();
    let open = client
        .open_menu(window, anchor, &menu)
        .expect("the open is accepted");

    let refused = WindowEvent::MenuClosed {
        window_id: window,
        open_id: open,
        outcome: MenuOutcome::Refused(MenuRefusal::NoResources),
    };
    assert_eq!(
        deliver(&loopback, &mut FullSink, &refused),
        Err(Errno::WouldBlock),
        "a full mailbox is relayed, not swallowed"
    );
    assert_eq!(
        client.open_menu(window, anchor, &menu),
        Err(Errno::AlreadyExists),
        "the open is still owed, so no second chain starts"
    );

    let mut sink = QueueSink::default();
    deliver(&loopback, &mut sink, &refused).expect("an accepted outcome answers it");
    assert_eq!(sink.delivered.len(), 1);
    client
        .open_menu(window, anchor, &menu)
        .expect("the window may open again");
}

/// A desktop that composes no menu service refuses an open rather than
/// accepting one nothing will ever answer.
///
/// This is the trait's own default, exercised through a host implementing
/// only the mandatory bridge methods: a refused menu is an answer the
/// application reports and carries on from, never a chain left owed.
#[test]
fn a_host_with_no_menu_service_refuses_an_open() {
    let mapper = MockMapper::with_regions(&[(7, FRAME_LEN)]);
    let mut server = WindowServer::new(mapper, SERVER, CLIENT_FRAME_MAX);
    let mut host = MinimalHost;
    let mut identity = MockIdentity::holding_layer(&[]);
    let mut reply = [0u8; WINDOW_REPLY_MAX];

    let create = request_frame(&WindowRequest::Create {
        shm_handle: 7,
        event_endpoint: EVENTS_A,
        frame_count: 1,
        width_px: SURFACE.width_px,
        height_px: SURFACE.height_px,
        stride_bytes: SURFACE.stride_bytes,
        format: SURFACE.format,
        title: tairix_abi::window_ipc::WindowTitle::new("a").expect("valid title"),
        sizing: WindowSizing::Fixed,
    });
    let outcome = server.serve(
        &mut host,
        &mut QueueSink::default(),
        &mut identity,
        TICKET_A,
        &create,
        &mut reply,
    );
    let len = replied(outcome);
    let (window, _) = tairix_abi::window_ipc::decode_create_reply(&reply[..len]).expect("created");

    let open = request_frame(&WindowRequest::OpenMenu {
        window_id: window,
        anchor: sample_menu_anchor(),
        menu: sample_open_menu(),
    });
    let outcome = server.serve(
        &mut host,
        &mut QueueSink::default(),
        &mut identity,
        TICKET_A,
        &open,
        &mut reply,
    );
    let len = replied(outcome);
    assert_eq!(
        tairix_abi::window_ipc::decode_minted_id_reply(&reply[..len]),
        Err(Errno::NotSupported)
    );
}

/// An icon-bar row's committed text is held for that application and that
/// row alone, answered once, and replaced by the row's next commit; two rows
/// committed one after the other never answer for each other.
#[test]
fn committed_app_bar_text_is_held_per_row_and_taken_once() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let row = |raw| AppMenuItemId::new(raw).expect("a valid id");
    let record = |item, text: &str| {
        loopback
            .borrow_mut()
            .server
            .record_app_bar_text(proc_id(0xA1), item, 0, text)
    };
    assert_eq!(
        record(row(4), "Disc"),
        Err(Errno::NotFound),
        "no presence, nothing to hold for"
    );
    client
        .set_app_bar(&sample_app_bar(EVENTS_A))
        .expect("declared");
    let over = "n".repeat(APP_MENU_ENTRY_MAX + 1);
    assert_eq!(record(row(4), &over), Err(Errno::LengthOutOfRange));
    record(row(4), "First").expect("held");
    record(row(4), "Archive").expect("the row's next commit replaces it");
    record(row(5), "Photos").expect("another row holds its own");
    assert_eq!(client.take_app_bar_text(row(9), 0), Ok(None));
    assert_eq!(
        client.take_app_bar_text(row(5), 0),
        Ok(Some(String::from("Photos")))
    );
    assert_eq!(
        client.take_app_bar_text(row(4), 0),
        Ok(Some(String::from("Archive")))
    );
    assert_eq!(client.take_app_bar_text(row(4), 0), Ok(None), "taken once");

    record(row(6), "Kept").expect("held");
    loopback.borrow_mut().ticket = TICKET_B;
    assert_eq!(
        client.take_app_bar_text(row(6), 0),
        Ok(None),
        "another application reads none of it"
    );
    loopback.borrow_mut().ticket = TICKET_A;
    for raw in 100..100 + APP_MENU_MAX_TOTAL_ROWS {
        let raw = u16::try_from(raw).expect("small");
        let _ = record(row(raw), "x");
    }
    assert_eq!(
        record(row(999), "one too many"),
        Err(Errno::LimitExceeded),
        "bounded by the rows a declaration carries"
    );
    record(row(6), "replaced").expect("a held row is replaced at the bound");
}

/// A row's held text answers only a take naming the revision it was committed
/// under, so an answer from an earlier declaration's menu never collects a
/// later commit; the commit stays held for the take that names it.
#[test]
fn committed_app_bar_text_answers_only_its_own_revision() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let row = AppMenuItemId::new(4).expect("a valid id");
    client
        .set_app_bar(&sample_app_bar(EVENTS_A))
        .expect("declared");
    loopback
        .borrow_mut()
        .server
        .record_app_bar_text(proc_id(0xA1), row, 2, "Archive")
        .expect("held");
    assert_eq!(
        client.take_app_bar_text(row, 1),
        Ok(None),
        "a take naming another revision collects nothing"
    );
    assert_eq!(
        client.take_app_bar_text(row, 2),
        Ok(Some(String::from("Archive")))
    );
}

/// The declaration a test application makes: it handles the click and
/// offers one chooseable row plus the session-rendered information row.
fn sample_app_bar(endpoint: u64) -> AppBar {
    let mut menu = AppMenu::EMPTY;
    menu.push(AppMenuRow::Item(AppMenuItem::new(
        AppMenuItemId::new(1).expect("a valid id"),
        AppMenuLabel::new("New window").expect("a valid label"),
        AppMenuSymbol::NewWindow,
    )))
    .expect("room");
    menu.push(AppMenuRow::Info).expect("room");
    AppBar {
        event_endpoint: endpoint,
        click: AppBarClick::Open,
        menu,
        revision: 0,
    }
}

/// A client encodes every request into one buffer it holds for its own
/// lifetime, so a short request issued after a wide one is byte-for-byte the
/// request it would have been on its own.
///
/// The buffer is sized to the widest operation the channel has, which is why
/// it is not taken per call — a present, the hottest operation and one of the
/// shortest, would otherwise clear the whole of a declaration's width every
/// composited frame. Reuse is only safe because a frame is written and sent
/// at its own operation's length, and this is what holds that.
#[test]
fn reusing_the_encode_buffer_leaks_nothing_into_a_later_frame() {
    let fresh = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut first = WindowClient::new(Rc::clone(&fresh));
    let (window, _) = first
        .create(7, EVENTS_A, 1, &SURFACE, "Files", WindowSizing::Fixed)
        .expect("a window");
    first
        .present(window, 0, full_damage())
        .expect("the first present");
    let alone = fresh
        .borrow()
        .sent
        .last()
        .cloned()
        .expect("a present frame");

    // The same present, on a client that has since sent the widest operation
    // the channel has.
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let (window, _) = client
        .create(7, EVENTS_A, 1, &SURFACE, "Files", WindowSizing::Fixed)
        .expect("a window");
    client
        .set_app_bar(&sample_app_bar(EVENTS_A))
        .expect("declared");
    let widest = loopback
        .borrow()
        .sent
        .last()
        .cloned()
        .expect("a declaration frame");
    client
        .present(window, 0, full_damage())
        .expect("the second present");
    let after = loopback.borrow().sent.last().cloned().expect("a frame");

    assert!(
        widest.len() > alone.len(),
        "the declaration is the wider operation, or this proves nothing"
    );
    assert_eq!(after, alone, "a present carries only its own bytes");
}

#[test]
fn an_icon_bar_declaration_reaches_the_host_and_routes_its_events() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let bar = sample_app_bar(EVENTS_A);

    // The declaration needs no window: an application claims a slot under
    // its own attested identity, whether or not it has anything open.
    client.set_app_bar(&bar).expect("declared");
    assert_eq!(
        loopback.borrow().host.app_bars,
        alloc::vec![(proc_id(0xA1), bar)]
    );

    // Its events route to the endpoint the declaration named, and carry no
    // window id.
    let mut sink = QueueSink::default();
    for event in [
        WindowEvent::AppBarDefault,
        WindowEvent::AppBarMenu {
            item: AppMenuItemId::new(1).expect("a valid id"),
            revision: 0,
        },
    ] {
        loopback
            .borrow_mut()
            .server
            .deliver_app_event(&mut sink, proc_id(0xA1), &event, GRANTED)
            .expect("delivered");
    }
    assert_eq!(
        sink.delivered,
        alloc::vec![
            (EVENTS_A, WindowEvent::AppBarDefault.to_le_bytes()),
            (
                EVENTS_A,
                WindowEvent::AppBarMenu {
                    item: AppMenuItemId::new(1).expect("a valid id"),
                    revision: 0,
                }
                .to_le_bytes()
            )
        ]
    );

    // A re-declaration replaces the route whole rather than accumulating
    // one, so an application that moves its mailbox is not delivered to
    // both.
    let moved = sample_app_bar(EVENTS_B);
    client.set_app_bar(&moved).expect("re-declared");
    let mut sink = QueueSink::default();
    loopback
        .borrow_mut()
        .server
        .deliver_app_event(
            &mut sink,
            proc_id(0xA1),
            &WindowEvent::AppBarDefault,
            GRANTED,
        )
        .expect("delivered");
    assert_eq!(
        sink.delivered,
        alloc::vec![(EVENTS_B, WindowEvent::AppBarDefault.to_le_bytes())]
    );
}

#[test]
fn declaring_an_icon_bar_presence_says_why_there_is_none() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let bar = sample_app_bar(EVENTS_A);
    assert_eq!(crate::declare_app_bar(&mut client, Ok(bar)), Ok(()));
    assert_eq!(
        loopback.borrow().host.app_bars,
        alloc::vec![(proc_id(0xA1), bar)]
    );

    loopback.borrow_mut().host.refuse_app_bar = Some(Errno::NotSupported);
    let refused = crate::declare_app_bar(&mut client, Ok(bar));
    assert_eq!(
        refused,
        Err(crate::AppBarRefused::Refused(Errno::NotSupported))
    );
    let said = alloc::format!("{}", refused.expect_err("refused"));
    assert!(said.contains("refused") && said.ends_with("carrying on without one"));

    let invalid = crate::declare_app_bar(&mut client, Err(Errno::LengthOutOfRange));
    assert_eq!(
        invalid,
        Err(crate::AppBarRefused::Invalid(Errno::LengthOutOfRange)),
        "an invalid declaration is never sent"
    );
    // A user reads the reason, never the variant's name.
    let said = alloc::format!("{}", invalid.expect_err("invalid"));
    assert!(
        said.contains(&alloc::format!("({})", Errno::LengthOutOfRange)),
        "{said}"
    );
}

#[test]
fn icon_bar_delivery_fails_closed_without_a_declaration() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let mut sink = QueueSink::default();

    // An application that never declared a presence has nowhere for a bar
    // event to go, so the delivery is refused rather than guessed at.
    assert_eq!(
        loopback.borrow_mut().server.deliver_app_event(
            &mut sink,
            proc_id(0xA1),
            &WindowEvent::AppBarDefault,
            GRANTED,
        ),
        Err(Errno::NotFound)
    );

    // A refused declaration records no route either: the engine remembers
    // one only once the host accepted it.
    loopback.borrow_mut().host.refuse_app_bar = Some(Errno::NotSupported);
    assert_eq!(
        client.set_app_bar(&sample_app_bar(EVENTS_A)),
        Err(Errno::NotSupported)
    );
    assert_eq!(
        loopback.borrow_mut().server.deliver_app_event(
            &mut sink,
            proc_id(0xA1),
            &WindowEvent::AppBarDefault,
            GRANTED,
        ),
        Err(Errno::NotFound)
    );
    loopback.borrow_mut().host.refuse_app_bar = None;

    // The two delivery paths are not interchangeable: a window-scoped
    // event on the application path (and the reverse) is a routing bug and
    // is refused, never delivered to the wrong place.
    client
        .set_app_bar(&sample_app_bar(EVENTS_A))
        .expect("declared");
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    assert_eq!(
        loopback.borrow_mut().server.deliver_app_event(
            &mut sink,
            proc_id(0xA1),
            &WindowEvent::CloseRequested { window_id: window },
            GRANTED,
        ),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        deliver(&loopback, &mut sink, &WindowEvent::AppBarDefault),
        Err(Errno::OutOfRange)
    );
    assert!(sink.delivered.is_empty());
}

#[test]
fn a_dead_clients_icon_bar_presence_is_withdrawn() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    client
        .set_app_bar(&sample_app_bar(EVENTS_A))
        .expect("declared");

    let owner = proc_id(0xA1);
    {
        let mut borrowed = loopback.borrow_mut();
        let Loopback { server, host, .. } = &mut *borrowed;
        server.client_exited(host, owner);
    }
    assert_eq!(
        loopback.borrow().host.app_bars_withdrawn,
        alloc::vec![owner]
    );

    // The route is gone with it, so a late bar event has nowhere to land.
    let mut sink = QueueSink::default();
    assert_eq!(
        loopback.borrow_mut().server.deliver_app_event(
            &mut sink,
            owner,
            &WindowEvent::AppBarDefault,
            GRANTED,
        ),
        Err(Errno::NotFound)
    );

    // A client that never declared one is not reported as withdrawing it.
    let other = proc_id(0xB2);
    {
        let mut borrowed = loopback.borrow_mut();
        let Loopback { server, host, .. } = &mut *borrowed;
        server.client_exited(host, other);
    }
    assert_eq!(
        loopback.borrow().host.app_bars_withdrawn,
        alloc::vec![owner]
    );
}

#[test]
fn a_wire_pointer_event_translates_to_the_position_then_the_button() {
    use crate::pointer_input_events;
    use tairix_geometry::Point;
    use tairix_input::{InputEvent, PointerButton};

    let at = Point::new(37, 91);

    // A bare move is one event: the position.
    let moved: Vec<InputEvent> = pointer_input_events(PointerAction::Moved, at).collect();
    assert_eq!(moved, [InputEvent::PointerMoved { to: at }]);

    // Every button transition is preceded by the position it happened at,
    // because the controls' press and release carry no coordinate.
    for (code, button) in [
        (PointerButtonCode::Primary, PointerButton::Primary),
        (PointerButtonCode::Secondary, PointerButton::Secondary),
        (PointerButtonCode::Middle, PointerButton::Middle),
    ] {
        let pressed: Vec<InputEvent> =
            pointer_input_events(PointerAction::Pressed(code), at).collect();
        assert_eq!(
            pressed,
            [
                InputEvent::PointerMoved { to: at },
                InputEvent::PointerPressed { button }
            ]
        );
        let released: Vec<InputEvent> =
            pointer_input_events(PointerAction::Released(code), at).collect();
        assert_eq!(
            released,
            [
                InputEvent::PointerMoved { to: at },
                InputEvent::PointerReleased { button }
            ]
        );
    }
}

/// A cancel is one event and moves nothing: a drag the desktop took stays
/// where it was left rather than jumping to where the window last saw the
/// pointer.
#[test]
fn a_wire_cancel_translates_to_one_cancel_and_no_move() {
    use crate::pointer_input_events;
    use tairix_geometry::Point;
    use tairix_input::InputEvent;

    let cancelled: Vec<InputEvent> =
        pointer_input_events(PointerAction::Cancelled, Point::new(5, 9)).collect();
    assert_eq!(cancelled, [InputEvent::PointerCancelled]);
}

#[test]
fn a_wire_scroll_translates_to_its_place_then_the_turn() {
    use crate::scroll_input_events;
    use tairix_geometry::Point;
    use tairix_input::InputEvent;

    let at = Point::new(12, 640);
    let delivered: Vec<InputEvent> = scroll_input_events(at, -15, 240).collect();
    assert_eq!(
        delivered,
        [
            InputEvent::PointerMoved { to: at },
            InputEvent::PointerScrolled { dx: -15, dy: 240 }
        ]
    );
}

#[test]
fn a_wire_pinch_translates_to_its_place_then_the_pinch_there() {
    use crate::pinch_input_events;
    use tairix_geometry::Point;
    use tairix_input::{InputEvent, PinchPhase};

    let at = Point::new(3, 70);
    let scale = 2 * tairix_abi::touch::PINCH_SCALE_ONE;
    let delivered: Vec<InputEvent> = pinch_input_events(at, PinchPhase::Update, scale).collect();
    assert_eq!(
        delivered,
        [
            InputEvent::PointerMoved { to: at },
            InputEvent::Pinch {
                phase: PinchPhase::Update,
                scale,
                at
            }
        ]
    );
}

#[test]
fn a_wire_key_event_translates_to_the_shared_key_vocabulary() {
    use crate::key_input_event;
    use tairix_abi::input::NamedKeyCode;
    use tairix_input::{InputEvent, Key, NamedKey};

    let held = Modifiers {
        shift: true,
        ctrl: false,
        alt: true,
        meta: false,
    };
    assert_eq!(
        key_input_event(KeyInput::Pressed {
            key: KeyValue::Named(NamedKeyCode::Tab),
            modifiers: held,
            physical: PhysicalKey::UNKNOWN,
        }),
        InputEvent::KeyPressed {
            key: Key::Named(NamedKey::Tab),
            modifiers: tairix_input::Modifiers {
                shift: true,
                ctrl: false,
                alt: true,
                meta: false,
            },
        }
    );
    // A release stays a release, and a character key carries its scalar.
    assert_eq!(
        key_input_event(KeyInput::Released {
            key: KeyValue::Char('q'),
            modifiers: Modifiers::default(),
            physical: PhysicalKey::UNKNOWN,
        }),
        InputEvent::KeyReleased {
            key: Key::Char('q'),
            modifiers: tairix_input::Modifiers::default(),
        }
    );
    // The function keys keep their number rather than collapsing together.
    assert_eq!(
        key_input_event(KeyInput::Pressed {
            key: KeyValue::Named(NamedKeyCode::F7),
            modifiers: Modifiers::default(),
            physical: PhysicalKey::UNKNOWN,
        }),
        InputEvent::KeyPressed {
            key: Key::Named(NamedKey::Function { number: 7 }),
            modifiers: tairix_input::Modifiers::default(),
        }
    );
}

#[test]
fn a_wire_pointer_position_widens_saturating_into_window_geometry() {
    assert_eq!(pointer_point(0, 0), Point::new(0, 0));
    assert_eq!(pointer_point(7, 11), Point::new(7, 11));
    // Past the signed range the coordinate saturates rather than wrapping onto
    // a control: the point lands outside every laid-out rectangle.
    let far = pointer_point(u32::MAX, u32::MAX);
    assert_eq!(far, Point::new(i32::MAX, i32::MAX));
    assert!(!Rect::new(0, 0, 4096, 4096).contains(far));
}

#[test]
fn a_reported_rect_becomes_the_damage_it_covers() {
    let rect = Rect::new(1, 1, 2, 2);
    let damage = damage_in(&SURFACE, rect).expect("inside the surface");
    assert_eq!(
        (damage.x, damage.y, damage.width_px, damage.height_px),
        (1, 1, 2, 2)
    );
}

#[test]
fn a_reported_rect_is_clipped_to_the_window() {
    // A control drawn partly off the window — a popup at the edge — must not
    // present pixels the session would refuse.
    let damage = damage_in(&SURFACE, Rect::new(-4, 2, 100, 100)).expect("the part inside");
    assert_eq!(
        (damage.x, damage.y, damage.width_px, damage.height_px),
        (0, 2, 4, 1)
    );
}

#[test]
fn a_rect_wholly_outside_the_window_presents_nothing() {
    assert_eq!(damage_in(&SURFACE, Rect::new(9, 9, 4, 4)), None);
    assert_eq!(damage_in(&SURFACE, Rect::EMPTY), None);
}

#[test]
fn a_round_s_conclusions_merge_to_the_strongest() {
    assert_eq!(Repaint::reported_if(true), Repaint::Reported);
    assert_eq!(Repaint::reported_if(false), Repaint::Nothing);
    let all = [Repaint::Nothing, Repaint::Reported, Repaint::Whole];
    for a in all {
        for b in all {
            let merged = a.merged(b);
            assert_eq!(merged, b.merged(a), "{a:?} with {b:?}");
            let expected = if a == Repaint::Whole || b == Repaint::Whole {
                Repaint::Whole
            } else if a == Repaint::Reported || b == Repaint::Reported {
                Repaint::Reported
            } else {
                Repaint::Nothing
            };
            assert_eq!(merged, expected, "{a:?} with {b:?}");
        }
    }
}

#[test]
fn a_round_that_changed_nothing_presents_nothing() {
    let damage = Region::new();
    assert_eq!(present_damage(&SURFACE, Repaint::Nothing, &damage), None);
}

#[test]
fn a_round_presents_what_it_reported() {
    let mut damage = Region::new();
    damage.add(Rect::new(1, 0, 1, 1));
    damage.add(Rect::new(3, 2, 1, 1));
    let presented = present_damage(&SURFACE, Repaint::Reported, &damage).expect("a rectangle");
    // One present per frame carries one rectangle, so two reports present the
    // box around them — still far short of the window where they are close.
    assert_eq!(
        (
            presented.x,
            presented.y,
            presented.width_px,
            presented.height_px
        ),
        (1, 0, 3, 3)
    );
}

/// A painter that paints each reported rectangle alone presents them as they
/// are, clipped to the window, so two far-apart edits are two rectangles
/// rather than the box between them.
#[test]
fn a_round_presents_each_rectangle_it_reported() {
    let mut damage = Region::new();
    damage.add(Rect::new(0, 0, 1, 1));
    damage.add(Rect::new(3, 2, 9, 9));
    let presented = present_damage_list(&SURFACE, Repaint::Reported, &damage).expect("rectangles");
    assert_eq!(presented.rects(), [rect(0, 0, 1, 1), rect(3, 2, 1, 1)]);
    assert_eq!(
        present_damage_list(&SURFACE, Repaint::Reported, &Region::new()).map(|list| list.bounds()),
        Some(DamageRect::full(&SURFACE)),
        "a round that reported nothing presents the window"
    );
    assert_eq!(
        present_damage_list(&SURFACE, Repaint::Nothing, &damage),
        None
    );
    assert_eq!(
        present_damage_list(&SURFACE, Repaint::Whole, &damage).map(|list| list.bounds()),
        Some(DamageRect::full(&SURFACE))
    );
}

/// A window of 200×100: room for a burst of separated edits.
const WIDE: DisplayMode = DisplayMode {
    width_px: 200,
    height_px: 100,
    stride_bytes: 800,
    format: DisplayFormat::Bgra8888,
};

/// One round's region of `rects`.
fn reported(rects: &[Rect]) -> Region {
    let mut region = Region::new();
    for rect in rects {
        region.add(*rect);
    }
    region
}

/// Rounds folded into one account present every rectangle any of them
/// reported, clipped to the window, and leave nothing owed once taken.
#[test]
fn owed_rounds_present_every_rectangle_once() {
    let mut owed = Owed::new();
    assert!(owed.is_clean());
    owed.owe(&WIDE, Repaint::Nothing, &reported(&[Rect::new(0, 0, 9, 9)]));
    assert!(owed.is_clean(), "a round that changed nothing owes nothing");
    owed.owe(
        &WIDE,
        Repaint::Reported,
        &reported(&[Rect::new(10, 10, 5, 5)]),
    );
    owed.owe(
        &WIDE,
        Repaint::Reported,
        &reported(&[Rect::new(190, 90, 50, 50)]),
    );
    assert!(!owed.is_clean() && !owed.is_whole());
    let parts = owed.take(&WIDE).expect("rectangles");
    assert_eq!(parts.rects(), [rect(10, 10, 5, 5), rect(190, 90, 10, 10)]);
    assert!(owed.is_clean());
    assert_eq!(owed.take(&WIDE), None);
}

/// However many rounds fold in, the account holds what one present carries,
/// still covers every reported pixel, and keeps two far-apart columns of
/// edits apart rather than presenting the area between them.
#[test]
fn a_burst_of_rounds_stays_bounded_and_keeps_far_edges_apart() {
    let mut owed = Owed::new();
    for step in 0..60 {
        let edits = [Rect::new(2, step, 1, 1), Rect::new(197, step, 1, 1)];
        owed.owe(&WIDE, Repaint::Reported, &reported(&edits));
    }
    let parts = owed.take(&WIDE).expect("rectangles");
    assert!(parts.len() <= tairix_abi::driver::display::MAX_DAMAGE_RECTS);
    let covers = |x: u32, y: u32| {
        parts.rects().iter().any(|part| {
            (part.x..part.x + part.width_px).contains(&x)
                && (part.y..part.y + part.height_px).contains(&y)
        })
    };
    assert!((0..60).all(|y| covers(2, y) && covers(197, y)));
    let covered: u64 = parts
        .rects()
        .iter()
        .map(|part| u64::from(part.width_px) * u64::from(part.height_px))
        .sum();
    assert!(
        covered <= 2 * 60,
        "the columns were merged across: {parts:?}"
    );
}

/// A reported round naming nothing inside the window still moved pixels, so
/// it owes the window whole, and a whole account absorbs every later round.
#[test]
fn an_owed_round_reporting_nothing_in_the_window_owes_it_whole() {
    let mut owed = Owed::new();
    owed.owe(
        &WIDE,
        Repaint::Reported,
        &reported(&[Rect::new(5, 5, 2, 2)]),
    );
    owed.owe(
        &WIDE,
        Repaint::Reported,
        &reported(&[Rect::new(300, 300, 4, 4)]),
    );
    assert!(owed.is_whole());
    owed.owe(
        &WIDE,
        Repaint::Reported,
        &reported(&[Rect::new(5, 5, 2, 2)]),
    );
    assert_eq!(
        owed.take(&WIDE).map(|parts| parts.rects().to_vec()),
        Some(alloc::vec![DamageRect::full(&WIDE)])
    );
    owed.owe(&WIDE, Repaint::Whole, &Region::new());
    owed.owe(&WIDE, Repaint::Nothing, &Region::new());
    assert!(owed.is_whole());
}

#[test]
fn a_round_that_changed_the_view_but_reported_nothing_presents_the_window() {
    // Under-covering would leave a stale frame on screen, so the fallback is
    // the whole window rather than nothing.
    let damage = Region::new();
    assert_eq!(
        present_damage(&SURFACE, Repaint::Reported, &damage),
        Some(DamageRect::full(&SURFACE))
    );
}

#[test]
fn a_whole_round_presents_the_window_whatever_was_reported() {
    let mut damage = Region::new();
    damage.add(Rect::new(1, 1, 1, 1));
    assert_eq!(
        present_damage(&SURFACE, Repaint::Whole, &damage),
        Some(DamageRect::full(&SURFACE))
    );
}

fn rect(x: u32, y: u32, width_px: u32, height_px: u32) -> DamageRect {
    DamageRect {
        x,
        y,
        width_px,
        height_px,
    }
}

/// A present into a region the session released sends the whole window, and
/// into a live one exactly the rectangles it was handed.
#[test]
fn a_present_after_a_release_sends_the_whole_window() {
    let parts = DamageList::new(&[rect(2, 1, 1, 1), rect(0, 0, 1, 1)]).expect("two rectangles");
    assert_eq!(
        presented_damage(&SURFACE, false, parts).map(|list| list.rects().to_vec()),
        Ok(parts.rects().to_vec())
    );
    assert_eq!(
        presented_damage(&SURFACE, true, parts).map(|list| list.rects().to_vec()),
        Ok(alloc::vec![DamageRect::full(&SURFACE)])
    );
}

#[test]
fn a_retained_present_resends_what_an_earlier_one_left_torn() {
    let torn = Some(rect(0, 0, 1, 1));
    assert_eq!(
        retained_damage(&SURFACE, false, torn, rect(2, 1, 1, 1)),
        Some(rect(0, 0, 3, 2))
    );
    assert_eq!(
        retained_damage(&SURFACE, false, None, rect(2, 1, 1, 1)),
        Some(rect(2, 1, 1, 1))
    );
}

#[test]
fn a_released_window_is_repainted_whole() {
    assert_eq!(
        retained_damage(&SURFACE, true, Some(rect(0, 0, 1, 1)), rect(2, 1, 1, 1)),
        Some(DamageRect::full(&SURFACE))
    );
}

/// A rectangle past the surface was once left torn as named when the frame
/// codec refused it, and every later present, widened over it, was refused
/// too until a resize: a window that ignored present errors froze silently.
#[test]
fn a_rectangle_past_the_surface_is_sent_clipped_so_it_cannot_poison_later_presents() {
    let first = retained_damage(&SURFACE, false, None, rect(2, 1, 50, 50));
    assert_eq!(first, Some(rect(2, 1, 2, 2)));
    let next = retained_damage(&SURFACE, false, first, rect(0, 0, 1, 1));
    assert_eq!(next, Some(rect(0, 0, 4, 3)));
    let edges = [0, 1, 3, 4, 5, u32::MAX - 1, u32::MAX];
    let mut named = Vec::new();
    for x in edges {
        for y in edges {
            for width in edges {
                named.extend(edges.map(|height| rect(x, y, width, height)));
            }
        }
    }
    for damage in named {
        let torn = retained_damage(&SURFACE, false, None, damage);
        for sent in [
            torn,
            retained_damage(&SURFACE, false, torn, rect(1, 1, 1, 1)),
        ]
        .into_iter()
        .flatten()
        {
            assert_eq!(sent.validate_in(&SURFACE), Ok(()), "{damage:?}");
        }
    }
}

#[test]
fn a_rectangle_wholly_outside_the_window_repaints_nothing() {
    assert_eq!(
        retained_damage(&SURFACE, false, None, rect(4, 0, 2, 2)),
        None
    );
    assert_eq!(
        retained_damage(&SURFACE, false, None, rect(0, 0, 0, 3)),
        None
    );
    assert_eq!(
        retained_damage(&SURFACE, false, None, rect(u32::MAX, 0, 8, 1)),
        None
    );
}

// ---- the open-target channel and the tooltip declaration ---------------

/// Client A's attested identity, which its own targets are queued under.
fn app_a() -> ProcId {
    proc_id(0xA1)
}

/// A path entry for `path`.
fn path_entry(path: &str) -> OpenEntry {
    OpenEntry::Path(String::from(path))
}

#[test]
fn an_open_target_is_queued_by_the_session_and_pulled_once_by_its_owner() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut sink = QueueSink::default();
    let mut client = WindowClient::new(Rc::clone(&loopback));
    create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");

    // Nothing queued is the honest empty answer, not an error.
    assert_eq!(client.take_open_target(), Ok(None));

    loopback
        .borrow_mut()
        .server
        .hand_over_open_target(&mut sink, app_a(), GRANTED, || {
            Ok(path_entry("Users:/ada/Documents"))
        })
        .expect("the application takes it");
    assert_eq!(
        client.take_open_target(),
        Ok(Some(Target::Path(String::from("Users:/ada/Documents"))))
    );
    assert_eq!(
        client.take_open_target(),
        Ok(None),
        "popping is what makes a target one-shot"
    );
}

/// An image the user opened is pulled as an image, never as the folder path
/// of the same spelling, and is bounded as a path is.
#[test]
fn an_image_hand_over_is_pulled_as_an_image_to_attach() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut sink = QueueSink::default();
    let mut client = WindowClient::new(Rc::clone(&loopback));
    create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    loopback
        .borrow_mut()
        .server
        .hand_over_open_target(&mut sink, app_a(), GRANTED, || {
            Ok(OpenEntry::Image(String::from("/Users/ada/Home/wb.adf")))
        })
        .expect("the application takes it");
    assert_eq!(
        client.take_open_target(),
        Ok(Some(Target::Image(String::from("/Users/ada/Home/wb.adf"))))
    );
    let long = "p".repeat(tairix_abi::FS_PATH_MAX + 1);
    assert_eq!(
        loopback
            .borrow_mut()
            .server
            .hand_over_open_target(&mut sink, app_a(), GRANTED, || {
                Ok(OpenEntry::Image(long.clone()))
            }),
        Err(Errno::LengthOutOfRange)
    );
}

#[test]
fn a_document_hand_over_carries_its_delegation_and_its_name() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut sink = QueueSink::default();
    let mut client = WindowClient::new(Rc::clone(&loopback));
    create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");

    loopback
        .borrow_mut()
        .server
        .hand_over_open_target(&mut sink, app_a(), GRANTED, || {
            Ok(OpenEntry::Document {
                name: DocumentName::new("holiday.png").expect("a name"),
                grant: 42,
                writable: true,
            })
        })
        .expect("the application takes it");
    assert_eq!(
        client.take_open_target(),
        Ok(Some(Target::Document {
            name: String::from("holiday.png"),
            grant: 42,
            writable: true,
        })),
        "a document is the form an application with no filesystem reach can open"
    );

    // A document with no delegation is nothing to open, so it never queues.
    assert_eq!(
        loopback
            .borrow_mut()
            .server
            .hand_over_open_target(&mut sink, app_a(), GRANTED, || Ok(OpenEntry::Document {
                name: DocumentName::new("holiday.png").expect("a name"),
                grant: 0,
                writable: false,
            }),),
        Err(Errno::OutOfRange)
    );
    assert_eq!(client.take_open_target(), Ok(None));
}

#[test]
fn one_delegation_handle_is_queued_once_however_often_it_is_handed_over() {
    // A grant handle is one-shot, and the kernel hands the *same* handle back
    // when the same authority is granted to the same process twice. Queueing
    // it twice would promise a second document the first pull consumes, so
    // the repeat is the answer rather than a second entry.
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut sink = QueueSink::default();
    let mut client = WindowClient::new(Rc::clone(&loopback));
    create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let document = OpenEntry::Document {
        name: DocumentName::new("holiday.png").expect("a name"),
        grant: 42,
        writable: false,
    };
    for _ in 0..3 {
        loopback
            .borrow_mut()
            .server
            .hand_over_open_target(&mut sink, app_a(), GRANTED, || Ok(document.clone()))
            .expect("a repeat is taken");
    }
    assert_eq!(
        client.take_open_target(),
        Ok(Some(Target::Document {
            name: String::from("holiday.png"),
            grant: 42,
            writable: false,
        }))
    );
    assert_eq!(client.take_open_target(), Ok(None), "one handle, one entry");
}

#[test]
fn handing_over_a_target_wakes_its_owner() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut sink = QueueSink::default();
    let mut client = WindowClient::new(Rc::clone(&loopback));
    create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");

    loopback
        .borrow_mut()
        .server
        .hand_over_open_target(&mut sink, app_a(), GRANTED, || {
            Ok(path_entry("Users:/ada/report"))
        })
        .expect("the application takes it");
    let (endpoint, bytes) = sink.delivered.pop_front().expect("a wake was announced");
    assert_eq!(endpoint, EVENTS_A);
    assert_eq!(
        WindowEvent::from_bytes(&bytes),
        Ok(WindowEvent::OpenRequested),
        "the wake names the application and carries no target"
    );
    assert!(sink.delivered.is_empty(), "one target, one wake");
}

#[test]
fn a_windowless_application_is_reached_through_its_icon_bar_route() {
    // The instance a hand-over most needs to reach: a resident
    // single-instance application sitting on the icon bar with nothing open.
    // A window-scoped queue would leave it unreachable.
    let loopback = Loopback::with_regions(&[]);
    let mut sink = QueueSink::default();
    let mut client = WindowClient::new(Rc::clone(&loopback));
    client
        .set_app_bar(&sample_app_bar(EVENTS_A))
        .expect("the bar declaration is accepted");

    loopback
        .borrow_mut()
        .server
        .hand_over_open_target(&mut sink, app_a(), GRANTED, || {
            Ok(path_entry("Users:/ada/report"))
        })
        .expect("an application with no window still has a route");
    let (endpoint, bytes) = sink.delivered.pop_front().expect("a wake was announced");
    assert_eq!(endpoint, EVENTS_A);
    assert_eq!(
        WindowEvent::from_bytes(&bytes),
        Ok(WindowEvent::OpenRequested)
    );
    assert_eq!(
        client.take_open_target(),
        Ok(Some(Target::Path(String::from("Users:/ada/report"))))
    );
}

/// A producer that must not run: producing a document entry mints a
/// delegation nothing can take back.
fn unproduced() -> Result<OpenEntry, Errno> {
    panic!("the entry was produced for an instance that did not take it")
}

#[test]
fn an_application_with_no_route_at_all_takes_nothing() {
    // No window and no icon-bar presence is no live instance to hand
    // anything to: the session is told so and starts a fresh process
    // instead of stranding a target.
    let loopback = Loopback::with_regions(&[]);
    let mut sink = QueueSink::default();
    assert_eq!(
        loopback
            .borrow_mut()
            .server
            .hand_over_open_target(&mut sink, app_a(), GRANTED, unproduced),
        Err(Errno::NotFound)
    );
    assert!(sink.delivered.is_empty());
}

#[test]
fn a_refused_wake_produces_and_queues_nothing() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");

    // A target the owner was never woken for would sit unreachable, and a
    // delegation minted for it could not be taken back.
    assert_eq!(
        loopback.borrow_mut().server.hand_over_open_target(
            &mut FullSink,
            app_a(),
            GRANTED,
            unproduced
        ),
        Err(Errno::WouldBlock)
    );
    assert_eq!(
        client.take_open_target(),
        Ok(None),
        "a refused hand-over strands no target"
    );
}

/// A document name the channel cannot carry is unrepresentable: its type
/// refuses it before a delegation could ever be minted for it, and the widest
/// one it carries is queued and pulled whole.
#[test]
fn a_document_name_the_channel_cannot_carry_cannot_be_queued() {
    assert_eq!(
        DocumentName::new(&"n".repeat(tairix_abi::FS_NAME_MAX + 1)),
        Err(Errno::LengthOutOfRange)
    );
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut sink = QueueSink::default();
    let mut client = WindowClient::new(Rc::clone(&loopback));
    create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let widest = DocumentName::new(&"n".repeat(tairix_abi::FS_NAME_MAX)).expect("fits");
    loopback
        .borrow_mut()
        .server
        .hand_over_open_target(&mut sink, app_a(), GRANTED, || {
            Ok(OpenEntry::Document {
                name: widest,
                grant: 5,
                writable: false,
            })
        })
        .expect("the widest name is carried");
    assert!(matches!(
        client.take_open_target(),
        Ok(Some(Target::Document { grant: 5, ref name, .. })) if name.len() == tairix_abi::FS_NAME_MAX
    ));
}

#[test]
fn queued_targets_are_pulled_oldest_first() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut sink = QueueSink::default();
    let mut client = WindowClient::new(Rc::clone(&loopback));
    create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    for path in ["Users:/one", "Users:/two", "Users:/three"] {
        loopback
            .borrow_mut()
            .server
            .hand_over_open_target(&mut sink, app_a(), GRANTED, || Ok(path_entry(path)))
            .expect("room");
    }
    for path in ["Users:/one", "Users:/two", "Users:/three"] {
        assert_eq!(
            client.take_open_target(),
            Ok(Some(Target::Path(String::from(path)))),
            "the ordering is the protocol"
        );
    }
    assert_eq!(client.take_open_target(), Ok(None));
}

#[test]
fn a_pull_reaches_only_the_callers_own_queue() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut sink = QueueSink::default();
    let mut client = WindowClient::new(Rc::clone(&loopback));
    create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    loopback
        .borrow_mut()
        .server
        .hand_over_open_target(&mut sink, app_a(), GRANTED, || {
            Ok(path_entry("Users:/ada/secret"))
        })
        .expect("room");

    // Another client's pull answers like a drained queue: the identity the
    // kernel attests is the scope, so the reply says nothing about who else
    // has something waiting.
    loopback.borrow_mut().ticket = TICKET_B;
    assert_eq!(client.take_open_target(), Ok(None));
    loopback.borrow_mut().ticket = TICKET_A;

    // And that pull took nothing: the owner still finds its target.
    assert_eq!(
        client.take_open_target(),
        Ok(Some(Target::Path(String::from("Users:/ada/secret"))))
    );
}

#[test]
fn the_open_target_queue_refuses_rather_than_dropping_or_growing() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut sink = QueueSink::default();
    let mut client = WindowClient::new(Rc::clone(&loopback));
    create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    for index in 0..WINDOW_MAX_OPEN_TARGETS {
        loopback
            .borrow_mut()
            .server
            .hand_over_open_target(&mut sink, app_a(), GRANTED, || {
                Ok(path_entry(&alloc::format!("Users:/{index}")))
            })
            .expect("within the bound");
    }
    assert_eq!(
        loopback
            .borrow_mut()
            .server
            .hand_over_open_target(&mut sink, app_a(), GRANTED, unproduced),
        Err(Errno::NoSpace),
        "the newest is refused rather than an older one dropped silently"
    );
    // The oldest is still first: a refusal at the far end disturbs nothing.
    assert_eq!(
        client.take_open_target(),
        Ok(Some(Target::Path(String::from("Users:/0"))))
    );

    // An empty or over-long path is refused too.
    assert_eq!(
        loopback
            .borrow_mut()
            .server
            .hand_over_open_target(&mut sink, app_a(), GRANTED, || Ok(path_entry(""))),
        Err(Errno::LengthOutOfRange)
    );
    let long = "p".repeat(tairix_abi::FS_PATH_MAX + 1);
    assert_eq!(
        loopback
            .borrow_mut()
            .server
            .hand_over_open_target(&mut sink, app_a(), GRANTED, || Ok(path_entry(&long))),
        Err(Errno::LengthOutOfRange)
    );
}

#[test]
fn an_applications_queued_targets_die_with_the_client() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut sink = QueueSink::default();
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    client
        .set_app_bar(&sample_app_bar(EVENTS_A))
        .expect("the bar declaration is accepted");
    loopback
        .borrow_mut()
        .server
        .hand_over_open_target(&mut sink, app_a(), GRANTED, || {
            Ok(path_entry("Users:/ada/report"))
        })
        .expect("room");

    // Closing the window leaves the queue alone: the application is still
    // there, still on the icon bar it declared, and the target is still its
    // to take.
    client.close(window).expect("the owner closes it");
    assert_eq!(
        client.take_open_target(),
        Ok(Some(Target::Path(String::from("Users:/ada/report")))),
        "a target outlives the window that happened to be open"
    );

    // The client going is what drops it: a target queued for a process that
    // has gone is reachable by nothing, and its delegation dies with it.
    loopback
        .borrow_mut()
        .server
        .hand_over_open_target(&mut sink, app_a(), GRANTED, || {
            Ok(path_entry("Users:/ada/report"))
        })
        .expect("room");
    let mut host = RecordingHost::default();
    loopback
        .borrow_mut()
        .server
        .client_exited(&mut host, app_a());
    assert_eq!(
        loopback
            .borrow_mut()
            .server
            .hand_over_open_target(&mut sink, app_a(), GRANTED, || Ok(path_entry(
                "Users:/ada/report"
            ))),
        Err(Errno::NotFound),
        "a client with no windows and no bar has no route left"
    );
    assert_eq!(
        client.take_open_target(),
        Ok(None),
        "and the queue went with it"
    );
}

#[test]
fn a_reveal_reaches_the_host_with_the_callers_attested_identity_and_its_answer_returns() {
    let loopback = Loopback::with_regions(&[]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    assert_eq!(client.reveal_document(5, 12), Ok(()));
    loopback.borrow_mut().host.reveal = Err(Errno::PermissionDenied);
    assert_eq!(
        client.reveal_document(5, 13),
        Err(Errno::PermissionDenied),
        "a refusal is relayed as itself"
    );
    assert_eq!(
        loopback.borrow().host.reveals,
        alloc::vec![(proc_id(0xA1), 5, 12), (proc_id(0xA1), 5, 13)],
        "the host sees who asked, from the kernel and not from the wire"
    );
}

/// An instance the desk cannot reach is answered `NotRunning`, whatever the
/// host believed: a launch that reached nothing must fall back to starting a
/// process rather than being reported as delivered.
#[test]
fn an_instance_the_desk_cannot_reach_is_answered_not_running() {
    let mut host = RecordingHost {
        hand_over: Ok(HandOverOutcome::Reached),
        ..RecordingHost::default()
    };
    let mut server = WindowServer::new(MockMapper::with_regions(&[]), SERVER, CLIENT_FRAME_MAX);
    let mut reply = [0u8; WINDOW_REPLY_MAX];
    let frame = request_frame(&WindowRequest::HandOverLaunch {
        run_path: tairix_abi::window_ipc::BundleRunPath::new("/System/Applications/view.app/Run")
            .expect("a valid bundle path"),
        target: HandOverTarget::Bare,
    });
    let outcome = server.serve(
        &mut host,
        &mut QueueSink::default(),
        &mut MockIdentity::holding_layer(&[]),
        TICKET_A,
        &frame,
        &mut reply,
    );
    let len = replied(outcome);
    assert_eq!(
        tairix_abi::window_ipc::decode_hand_over_reply(&reply[..len]),
        Ok(HandOverOutcome::NotRunning),
        "an unreachable instance is not a reached one"
    );
}

#[test]
fn a_hand_over_reaches_the_host_with_the_callers_attested_identity() {
    let loopback = Loopback::with_regions(&[]);
    let mut client = WindowClient::new(Rc::clone(&loopback));

    // The host answers "nothing to reach" by default, which is the caller's
    // cue to launch the bundle itself rather than a refusal.
    assert_eq!(
        client.hand_over_launch("/System/Applications/view.app/Run", HandOverTarget::Bare),
        Ok(HandOverOutcome::NotRunning)
    );
    let document = HandOverDocument {
        name: DocumentName::new("holiday.png").expect("a valid name"),
        grant: core::num::NonZeroU64::new(31).expect("a handle"),
        writable: false,
    };
    // A host that reaches an instance queues through the desk the engine
    // lends it, so the instance needs a route for the wake to reach.
    client
        .set_app_bar(&sample_app_bar(EVENTS_A))
        .expect("the bar declaration is accepted");
    loopback.borrow_mut().host.hand_over = Ok(HandOverOutcome::Reached);
    assert_eq!(
        client.hand_over_launch(
            "/System/Applications/view.app/Run",
            HandOverTarget::Document(document)
        ),
        Ok(HandOverOutcome::Reached)
    );
    assert_eq!(
        client.take_open_target(),
        Ok(Some(Target::Document {
            name: String::from("holiday.png"),
            grant: 31,
            writable: false,
        })),
        "the engine's half of the hand-over really queued it"
    );
    let pane = PaneName::new("files").expect("a pane");
    assert_eq!(
        client.hand_over_launch(tairix_abi::SETTINGS_RUN_PATH, HandOverTarget::Pane(pane)),
        Ok(HandOverOutcome::Reached)
    );
    assert_eq!(
        client.take_open_target(),
        Ok(Some(Target::Pane(String::from("files")))),
        "a pane reaches the instance as the place to show"
    );

    assert_eq!(
        loopback.borrow().host.hand_overs,
        alloc::vec![
            (
                proc_id(0xA1),
                String::from("/System/Applications/view.app/Run"),
                HandOverTarget::Bare
            ),
            (
                proc_id(0xA1),
                String::from("/System/Applications/view.app/Run"),
                HandOverTarget::Document(document)
            ),
            (
                proc_id(0xA1),
                String::from(tairix_abi::SETTINGS_RUN_PATH),
                HandOverTarget::Pane(pane)
            ),
        ],
        "the host sees who asked, from the kernel and not from the wire"
    );

    // A refusal is relayed as itself, and a path longer than a hand-over may
    // name never reaches the channel at all.
    loopback.borrow_mut().host.hand_over = Err(Errno::NoSpace);
    assert_eq!(
        client.hand_over_launch("/System/Applications/view.app/Run", HandOverTarget::Bare),
        Err(Errno::NoSpace)
    );
    let long = "p".repeat(HAND_OVER_RUN_PATH_MAX + 1);
    assert_eq!(
        client.hand_over_launch(&long, HandOverTarget::Bare),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(
        loopback.borrow().host.hand_overs.len(),
        4,
        "an over-long path is refused before the session is asked"
    );
}

#[test]
fn a_tooltip_declaration_reaches_the_host_and_replaces_rather_than_appends() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let region = sample_menu_anchor();
    let other = WindowRegion::new(4, 8, 20, 12).expect("a second region");

    // A window the caller does not own answers like one that never existed,
    // and the host is never told.
    loopback.borrow_mut().ticket = TICKET_B;
    client
        .set_tooltip(window, region, tip("Copy"))
        .expect("posted");
    assert_eq!(
        refusal(&mut client),
        Some(refused(window, PostedRequest::Tooltip, Errno::NotFound))
    );
    assert!(loopback.borrow().host.tooltips.is_empty());
    loopback.borrow_mut().ticket = TICKET_A;

    client
        .set_tooltip(window, region, tip("Copy"))
        .expect("the owner may declare one");
    client
        .set_tooltip(window, other, tip("Paste"))
        .expect("and re-declare it");
    client
        .set_tooltip(window, other, tip(""))
        .expect("and withdraw it");
    {
        let host = &loopback.borrow().host;
        assert_eq!(
            host.tooltips.len(),
            3,
            "each declaration is relayed; the host holds at most one at a time"
        );
        assert_eq!(host.tooltips[0], (window, region, String::from("Copy")));
        assert_eq!(host.tooltips[1], (window, other, String::from("Paste")));
        assert_eq!(
            host.tooltips[2],
            (window, other, String::new()),
            "empty text is the withdrawal"
        );
    }
}

#[test]
fn a_host_that_shows_no_tooltip_refuses_and_the_app_carries_on() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    loopback.borrow_mut().host.refuse_tooltip = Some(Errno::NotSupported);
    client
        .set_tooltip(window, sample_menu_anchor(), tip("Copy"))
        .expect("posted");
    assert_eq!(
        refusal(&mut client),
        Some(refused(window, PostedRequest::Tooltip, Errno::NotSupported)),
        "a refused tip is an answer the app reports and carries on from"
    );
    assert!(loopback.borrow().host.tooltips.is_empty());
}

#[test]
fn a_declared_tip_asks_once_per_tool_and_once_more_to_withdraw() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let (copy, paste) = (Rect::new(0, 0, 20, 20), Rect::new(20, 0, 20, 20));
    let mut tip = DeclaredTip::new();
    for _ in 0..3 {
        tip.declare(&mut client, window, Some((copy, "Copy")));
    }
    tip.declare(&mut client, window, Some((paste, "Paste")));
    tip.declare(&mut client, window, None);
    tip.declare(&mut client, window, None);
    let host = &loopback.borrow().host;
    assert_eq!(host.tooltips.len(), 3, "{:?}", host.tooltips);
    assert_eq!(host.tooltips[2].2, String::new(), "the withdrawal");
}

#[test]
fn a_declared_tip_asks_again_when_its_line_changes_over_the_same_region() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let host_plate = Rect::new(40, 40, 44, 44);
    let mut tip = DeclaredTip::new();
    tip.declare(
        &mut client,
        window,
        Some((host_plate, "printer — 1 service: ipp")),
    );
    tip.declare(
        &mut client,
        window,
        Some((host_plate, "printer — 2 services: http, ipp")),
    );
    let host = &loopback.borrow().host;
    assert_eq!(host.tooltips.len(), 2, "{:?}", host.tooltips);
    assert_eq!(host.tooltips[1].2, "printer — 2 services: http, ipp");
}

#[test]
fn a_tip_the_wire_cannot_carry_withdraws_the_last_one() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let mut tip = DeclaredTip::new();
    tip.declare(&mut client, window, Some((Rect::new(0, 0, 20, 20), "Copy")));
    let unsendable = "x".repeat(TOOLTIP_TEXT_MAX + 1);
    tip.declare(
        &mut client,
        window,
        Some((Rect::new(20, 0, 20, 20), unsendable.as_str())),
    );
    {
        let host = &loopback.borrow().host;
        assert_eq!(host.tooltips.len(), 2, "{:?}", host.tooltips);
        assert_eq!(
            host.tooltips[1].2,
            String::new(),
            "the line it could not send withdrew the one before it at once"
        );
    }
    tip.declare(&mut client, window, None);
    assert_eq!(
        loopback.borrow().host.tooltips.len(),
        2,
        "and nothing is left to withdraw"
    );
}

#[test]
fn a_refused_tip_is_not_asked_again_on_every_pointer_sample() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    loopback.borrow_mut().host.refuse_tooltip = Some(Errno::NotSupported);
    let asked = loopback.borrow().sent.len();
    let mut tip = DeclaredTip::new();
    for _ in 0..5 {
        tip.declare(&mut client, window, Some((Rect::new(0, 0, 20, 20), "Copy")));
    }
    assert_eq!(loopback.borrow().sent.len(), asked + 1);
}

/// An application raises its own window only under an activation a
/// user-driven application event handed it: spent by its first use, void once
/// the keyboard moves, never valid for another client's window, and gone with
/// the client.
#[test]
fn a_window_is_raised_on_request_only_under_an_activation_the_engine_minted() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    client
        .set_app_bar(&sample_app_bar(EVENTS_A))
        .expect("declared");
    let deliver_app = |event: &WindowEvent| {
        loopback
            .borrow_mut()
            .server
            .deliver_app_event(&mut QueueSink::default(), proc_id(0xA1), event, GRANTED)
            .expect("delivered");
    };

    assert_eq!(
        client.activate_window(window),
        Err(Errno::PermissionDenied),
        "nothing the user did asked for it"
    );
    deliver_app(&WindowEvent::AppBarDefault);
    assert_eq!(client.activate_window(window), Ok(()));
    assert_eq!(
        client.activate_window(window),
        Err(Errno::PermissionDenied),
        "an activation is spent by its first use"
    );

    deliver_app(&WindowEvent::AppBarMenu {
        item: AppMenuItemId::new(1).expect("a valid id"),
        revision: 0,
    });
    loopback
        .borrow_mut()
        .server
        .deliver_event(
            &mut QueueSink::default(),
            &WindowEvent::Focus {
                window_id: window,
                focused: true,
            },
        )
        .expect("delivered");
    assert_eq!(
        client.activate_window(window),
        Err(Errno::PermissionDenied),
        "the keyboard moved, so the gesture is past"
    );

    // A client already holding the keyboard restacks its own windows freely.
    loopback.borrow_mut().host.focused = Some(window);
    assert_eq!(client.activate_window(window), Ok(()));
    loopback.borrow_mut().host.focused = None;

    deliver_app(&WindowEvent::AppBarDefault);
    loopback.borrow_mut().ticket = TICKET_B;
    assert_eq!(client.activate_window(window), Err(Errno::NotFound));
    loopback.borrow_mut().ticket = TICKET_A;
    assert_eq!(
        loopback.borrow().host.raised,
        alloc::vec![window, window],
        "only the activated and the working requests reach the host"
    );

    loopback
        .borrow_mut()
        .server
        .client_exited(&mut RecordingHost::default(), proc_id(0xA1));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "b").expect("b");
    assert_eq!(
        client.activate_window(window),
        Err(Errno::PermissionDenied),
        "an activation dies with the client it was handed to"
    );
}

/// An activation is bound to where the keyboard was when it was minted: once
/// the keyboard has moved by any path, with or without a focus event, it
/// raises nothing, and one withheld raises nothing at all. Each refusal is
/// reported to the host.
#[test]
fn an_activation_raises_nothing_once_the_keyboard_has_moved_or_when_withheld() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    client
        .set_app_bar(&sample_app_bar(EVENTS_A))
        .expect("declared");
    let deliver_app = |activation| {
        loopback
            .borrow_mut()
            .server
            .deliver_app_event(
                &mut QueueSink::default(),
                proc_id(0xA1),
                &WindowEvent::AppBarDefault,
                activation,
            )
            .expect("delivered");
    };

    let elsewhere = window + 100;
    loopback.borrow_mut().host.focused = Some(elsewhere);
    deliver_app(Activation::Granted {
        focus: Some(elsewhere),
    });
    loopback.borrow_mut().host.focused = None;
    assert_eq!(
        client.activate_window(window),
        Err(Errno::PermissionDenied),
        "the keyboard left the window it was at"
    );

    deliver_app(Activation::Withheld);
    assert_eq!(client.activate_window(window), Err(Errno::PermissionDenied));

    deliver_app(GRANTED);
    deliver_app(Activation::Withheld);
    assert_eq!(
        client.activate_window(window),
        Err(Errno::PermissionDenied),
        "a later withheld event takes back an unspent grant"
    );

    let host = &loopback.borrow().host;
    assert!(host.raised.is_empty());
    assert_eq!(host.raises_refused, alloc::vec![(proc_id(0xA1), window); 3]);
}

/// A hand-over spends the activation a gesture at its caller minted: once,
/// and only where the keyboard still is, so the host can lend that one raise
/// to what the caller hands over to instead.
#[test]
fn a_hand_over_spends_the_activation_a_gesture_at_its_caller_minted() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    client
        .set_app_bar(&sample_app_bar(EVENTS_A))
        .expect("declared");
    let deliver_app = |activation| {
        loopback
            .borrow_mut()
            .server
            .deliver_app_event(
                &mut QueueSink::default(),
                proc_id(0xA1),
                &WindowEvent::AppBarDefault,
                activation,
            )
            .expect("delivered");
    };
    let settings = tairix_abi::SETTINGS_RUN_PATH;

    assert_eq!(
        client.hand_over_launch(settings, HandOverTarget::Bare),
        Ok(HandOverOutcome::NotRunning)
    );
    deliver_app(GRANTED);
    for _ in 0..2 {
        assert_eq!(
            client.hand_over_launch(settings, HandOverTarget::Bare),
            Ok(HandOverOutcome::NotRunning)
        );
    }
    deliver_app(Activation::Granted {
        focus: Some(window),
    });
    assert_eq!(
        client.hand_over_launch(settings, HandOverTarget::Bare),
        Ok(HandOverOutcome::NotRunning)
    );
    assert_eq!(
        loopback.borrow().host.spent,
        alloc::vec![false, true, false, false],
        "nothing to spend, the gesture's one raise, nothing left, one the keyboard has left"
    );
    assert_eq!(
        client.activate_window(window),
        Err(Errno::PermissionDenied),
        "a spent activation raises nothing"
    );
}

#[test]
fn only_a_window_s_owner_sets_its_pointer_shape() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    loopback.borrow_mut().ticket = TICKET_B;
    client
        .set_cursor(window, CursorShape::Text)
        .expect("posted");
    assert_eq!(
        refusal(&mut client),
        Some(refused(window, PostedRequest::Cursor, Errno::NotFound))
    );
    assert!(
        loopback.borrow().host.cursors.is_empty(),
        "another client's request never reaches the host"
    );
    loopback.borrow_mut().ticket = TICKET_A;
    client
        .set_cursor(window, CursorShape::Text)
        .expect("the owner sets it");
    client
        .set_cursor(window, CursorShape::Arrow)
        .expect("and restates it");
    assert_eq!(
        loopback.borrow().host.cursors,
        [(window, CursorShape::Text), (window, CursorShape::Arrow)]
    );
}

/// A tooltip's text, which the tests state as a plain literal.
fn tip(text: &str) -> TooltipText {
    TooltipText::new(text).expect("a valid tip")
}

/// A one-frame SURFACE-shaped layer surface granted as `shm`, its events
/// routed to `events`, opening at `(x, y)` in `depth`.
fn layer_spec(shm: u64, events: u64, x: i32, y: i32, depth: LayerDepth) -> LayerSpec {
    LayerSpec {
        shm_handle: shm,
        event_endpoint: events,
        frame_count: 1,
        surface: SURFACE,
        x,
        y,
        depth,
    }
}

#[test]
fn opening_a_layer_surface_needs_the_capability_and_reaches_the_host() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));

    // The gate is the kernel's attestation of the caller, not anything the
    // caller said: with the capability withheld the open is refused before
    // the host is told anything at all.
    loopback.borrow_mut().identity = MockIdentity::holding_layer(&[]);
    assert_eq!(
        client
            .open_layer(&layer_spec(7, EVENTS_A, 10, 20, LayerDepth::Above))
            .map(|(id, _)| id),
        Err(Errno::PermissionDenied)
    );
    assert!(
        loopback.borrow().host.layers.is_empty(),
        "a refused open must not reach the host"
    );

    // Granted, the open carries the attested owner, the geometry, the
    // screen point, and the depth through to the host.
    loopback.borrow_mut().identity = MockIdentity::holding_layer(&[TICKET_A]);
    let (id, _) = client
        .open_layer(&layer_spec(7, EVENTS_A, 10, 20, LayerDepth::Above))
        .expect("a held capability opens the surface");
    let host = &loopback.borrow().host;
    assert_eq!(
        host.layers,
        [(proc_id(0xA1), id, SURFACE, 10, 20, LayerDepth::Above)]
    );
}

#[test]
fn a_failed_capability_attestation_refuses_rather_than_grants() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    loopback.borrow_mut().identity.attest_error = Some(Errno::NotFound);
    assert_eq!(
        client
            .open_layer(&layer_spec(7, EVENTS_A, 0, 0, LayerDepth::Below))
            .map(|(id, _)| id),
        Err(Errno::NotFound),
        "an attestation the kernel could not answer fails closed"
    );
    assert!(loopback.borrow().host.layers.is_empty());
}

#[test]
fn every_layer_operation_is_gated_not_only_the_open() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let (id, _) = client
        .open_layer(&layer_spec(7, EVENTS_A, 0, 0, LayerDepth::Below))
        .expect("opened while held");

    // Revoking the grant stops the surface at its next request rather than
    // letting it run as long as the process does.
    loopback.borrow_mut().identity = MockIdentity::holding_layer(&[]);
    assert_eq!(
        client.place_layer(id, 5, 5, LayerDepth::Above),
        Err(Errno::PermissionDenied)
    );
    let mut plates = [TerrainPlate {
        x: 0,
        y: 0,
        width_px: 1,
        height_px: 1,
    }; 4];
    assert_eq!(
        client.take_terrain(id, &mut plates).map(<[_]>::len),
        Err(Errno::PermissionDenied)
    );
    assert!(loopback.borrow().host.layer_places.is_empty());
}

#[test]
fn a_layer_surface_is_one_per_client() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    client
        .open_layer(&layer_spec(7, EVENTS_A, 0, 0, LayerDepth::Below))
        .expect("the first is allowed");
    assert_eq!(DESKTOP_LAYER_MAX_PER_CLIENT, 1);
    assert_eq!(
        client
            .open_layer(&layer_spec(8, EVENTS_A, 0, 0, LayerDepth::Below))
            .map(|(id, _)| id),
        Err(Errno::LimitExceeded),
        "a second would multiply the screen area one holder occupies"
    );
}

#[test]
fn closing_a_layer_surface_frees_its_slot() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let (id, _) = client
        .open_layer(&layer_spec(7, EVENTS_A, 0, 0, LayerDepth::Below))
        .expect("the first is allowed");

    // Retiring is the ordinary `Close`: a layer surface is a window in the
    // one registry, so it needs no teardown path of its own.
    client.close(id).expect("closed");
    assert!(loopback.borrow().host.closed.contains(&id));
    client
        .open_layer(&layer_spec(8, EVENTS_A, 0, 0, LayerDepth::Below))
        .expect("the slot is free again");
}

#[test]
fn place_layer_is_owner_bound_and_refuses_an_ordinary_window() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let (layer, _) = client
        .open_layer(&layer_spec(7, EVENTS_A, 0, 0, LayerDepth::Below))
        .expect("opened");
    let window = create_id(&mut client, 8, EVENTS_A, 1, "a").expect("a window");

    // An ordinary window is placed by the window manager; letting this move
    // one would hand every holder authority over its own windows' positions.
    assert_eq!(
        client.place_layer(window, 1, 2, LayerDepth::Above),
        Err(Errno::NotSupported)
    );
    // A window nobody owns here leaks nothing beyond "not found".
    assert_eq!(
        client.place_layer(9_999, 1, 2, LayerDepth::Above),
        Err(Errno::NotFound)
    );
    client
        .place_layer(layer, 3, 4, LayerDepth::Above)
        .expect("its own surface moves");
    assert_eq!(
        loopback.borrow().host.layer_places,
        [(layer, 3, 4, LayerDepth::Above)]
    );
}

#[test]
fn a_refused_place_leaves_the_recorded_depth_alone() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let (layer, _) = client
        .open_layer(&layer_spec(7, EVENTS_A, 0, 0, LayerDepth::Below))
        .expect("opened");

    // The engine records the depth only once the host accepted it, so the
    // two can never disagree about what is actually stacked.
    loopback.borrow_mut().host.refuse_layer = Some(Errno::WouldBlock);
    assert_eq!(
        client.place_layer(layer, 1, 1, LayerDepth::Above),
        Err(Errno::WouldBlock)
    );
    loopback.borrow_mut().host.refuse_layer = None;
    client
        .place_layer(layer, 1, 1, LayerDepth::Above)
        .expect("accepted once the host can");
}

#[test]
fn terrain_is_answered_for_a_layer_surface_and_refused_for_anything_else() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let (layer, _) = client
        .open_layer(&layer_spec(7, EVENTS_A, 0, 0, LayerDepth::Below))
        .expect("opened");
    let window = create_id(&mut client, 8, EVENTS_A, 1, "a").expect("a window");

    let reported = [
        TerrainPlate {
            x: 0,
            y: 0,
            width_px: 100,
            height_px: 40,
        },
        TerrainPlate {
            x: 30,
            y: 60,
            width_px: 20,
            height_px: 20,
        },
    ];
    loopback.borrow_mut().host.terrain = reported.to_vec();

    let mut plates = [TerrainPlate {
        x: 0,
        y: 0,
        width_px: 1,
        height_px: 1,
    }; 8];
    assert_eq!(client.take_terrain(layer, &mut plates), Ok(&reported[..]));

    // An ordinary window has no terrain to ask about: the desktop's shape
    // is what the layer authority buys, not something every window may read.
    assert_eq!(
        client.take_terrain(window, &mut plates).map(<[_]>::len),
        Err(Errno::NotSupported)
    );
}

#[test]
fn a_refused_terrain_query_reaches_the_caller_as_a_refusal() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let (layer, _) = client
        .open_layer(&layer_spec(7, EVENTS_A, 0, 0, LayerDepth::Below))
        .expect("opened");
    loopback.borrow_mut().host.refuse_terrain = Some(Errno::SeatRevoked);
    let mut plates = [TerrainPlate {
        x: 0,
        y: 0,
        width_px: 1,
        height_px: 1,
    }; 4];
    assert_eq!(
        client.take_terrain(layer, &mut plates).map(<[_]>::len),
        Err(Errno::SeatRevoked),
        "a refusal must not read as an empty desktop"
    );
}

#[test]
fn a_host_that_has_not_implemented_the_layer_refuses_it() {
    // The default host bridge answers `NotSupported` rather than silently
    // accepting: privileged authority a host never implemented must not
    // appear to be granted.
    struct BareHost;
    impl WindowHost for BareHost {
        fn window_opened(
            &mut self,
            _owner: ProcId,
            _window_id: u64,
            _surface: &DisplayMode,
            _title: &str,
            _sizing: WindowSizing,
        ) -> Result<(), Errno> {
            Ok(())
        }

        fn window_resized(&mut self, _window_id: u64, _surface: &DisplayMode) -> Result<(), Errno> {
            Ok(())
        }

        fn window_sizing_changed(
            &mut self,
            _window_id: u64,
            _sizing: WindowSizing,
        ) -> Result<(), Errno> {
            Ok(())
        }

        fn window_size_state_changed(
            &mut self,
            _window_id: u64,
            _state: WindowSizeState,
        ) -> Result<(), Errno> {
            Ok(())
        }

        fn window_retitled(&mut self, _window_id: u64, _title: &str) -> Result<(), Errno> {
            Ok(())
        }

        fn pick_requested(
            &mut self,
            _window_id: u64,
            _purpose: &PickPurpose,
            _focus: bool,
        ) -> Result<(), Errno> {
            Ok(())
        }

        fn window_closed(&mut self, _window_id: u64) {}

        fn desktop(&mut self) -> Result<DesktopInfo, Errno> {
            Ok(sample_desktop())
        }
    }

    let mut server = WindowServer::new(
        MockMapper::with_regions(&[(7, FRAME_LEN)]),
        SERVER,
        CLIENT_FRAME_MAX,
    );
    let mut reply = [0u8; WINDOW_REPLY_MAX];
    let request = WindowRequest::OpenLayer {
        shm_handle: 7,
        event_endpoint: EVENTS_A,
        frame_count: 1,
        width_px: SURFACE.width_px,
        height_px: SURFACE.height_px,
        stride_bytes: SURFACE.stride_bytes,
        format: SURFACE.format,
        x: 0,
        y: 0,
        depth: LayerDepth::Above,
    };
    let mut frame = [0u8; 128];
    let len = request.encode(&mut frame).expect("encodes");
    let outcome = server.serve(
        &mut BareHost,
        &mut QueueSink::default(),
        &mut MockIdentity::holding_layer(&[TICKET_A]),
        TICKET_A,
        &frame[..len],
        &mut reply,
    );
    let written = replied(outcome);
    assert_eq!(
        decode_status_reply(&reply[..written.min(4)]),
        Err(Errno::NotSupported)
    );
}

/// An alert a test application raises: one sentence and one answer.
fn sample_alert() -> AlertSpec {
    AlertSpec::new(
        AlertSeverity::Error,
        "The folder could not be listed",
        "",
        &[AlertButton::new("OK", AlertButtonRole::Default).expect("a valid label")],
    )
    .expect("a valid alert")
}

/// The answers delivered to `endpoint` so far, decoded.
fn alert_answers(sink: &QueueSink, endpoint: u64) -> Vec<WindowEvent> {
    sink.delivered
        .iter()
        .filter(|(to, _)| *to == endpoint)
        .filter_map(|(_, frame)| WindowEvent::from_bytes(frame).ok())
        .filter(|event| matches!(event, WindowEvent::AlertClosed { .. }))
        .collect()
}

#[test]
fn an_alert_is_put_up_for_its_owner_and_answered_exactly_once() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let alert = client
        .open_alert(EVENTS_A, Some(window), &sample_alert())
        .expect("the alert is accepted");
    assert_ne!(alert, 0, "an alert id is never zero");
    {
        let host = &loopback.borrow().host;
        assert_eq!(host.alerts.len(), 1);
        let (id, owner, held, spec) = &host.alerts[0];
        assert_eq!((*id, *held), (alert, Some(window)));
        assert_eq!(*owner, proc_id(0xA1));
        assert_eq!(spec.headline(), "The folder could not be listed");
    }
    let mut sink = QueueSink::default();
    loopback
        .borrow_mut()
        .server
        .answer_alert(&mut sink, alert, 0, Activation::Withheld)
        .expect("the answer delivers");
    assert_eq!(
        alert_answers(&sink, EVENTS_A),
        [WindowEvent::AlertClosed {
            alert_id: alert,
            outcome: AlertOutcome::Chosen { index: 0 }
        }]
    );
    assert_eq!(
        loopback
            .borrow_mut()
            .server
            .answer_alert(&mut sink, alert, 0, Activation::Withheld),
        Err(Errno::NotFound),
        "an answered alert is answered no more"
    );
    let again = client
        .open_alert(EVENTS_A, Some(window), &sample_alert())
        .expect("a second alert is accepted");
    assert_ne!(again, alert, "an alert id is never reused");
}

#[test]
fn an_alert_may_hold_only_the_callers_own_top_level_window_or_dialog() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN), (9, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    loopback.borrow_mut().ticket = TICKET_B;
    assert_eq!(
        client.open_alert(EVENTS_B, Some(window), &sample_alert()),
        Err(Errno::NotFound)
    );
    loopback.borrow_mut().ticket = TICKET_A;
    let (popup, _) = client
        .create_popup(&popup_spec(window, 8, EVENTS_A, 0, 0))
        .expect("a popup");
    let (tool, _) = client
        .create_tool(&tool_spec(window, 8, None))
        .expect("a tool window");
    for fleeting in [popup, tool] {
        assert_eq!(
            client.open_alert(EVENTS_A, Some(fleeting), &sample_alert()),
            Err(Errno::OutOfRange)
        );
    }
    assert!(loopback.borrow().host.alerts.is_empty());
    // A copy's question holds the copy's own window, not the folder's.
    let (dialog, _) = client
        .create_dialog(&dialog_spec(window, 9, DialogModality::Modeless))
        .expect("a dialog");
    client
        .open_alert(EVENTS_A, Some(dialog), &sample_alert())
        .expect("an alert holds a dialog");
    assert_eq!(loopback.borrow().host.alerts[0].2, Some(dialog));
}

#[test]
fn an_application_with_no_window_may_still_raise_an_alert() {
    let loopback = Loopback::with_regions(&[]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let alert = client
        .open_alert(EVENTS_A, None, &sample_alert())
        .expect("an ownerless alert is accepted");
    assert_eq!(loopback.borrow().host.alerts[0].2, None);
    assert_ne!(alert, 0);
}

#[test]
fn an_application_has_at_most_its_bound_of_alerts_up() {
    let loopback = Loopback::with_regions(&[]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    for _ in 0..tairix_abi::window_ipc::ALERT_MAX_OPEN {
        client
            .open_alert(EVENTS_A, None, &sample_alert())
            .expect("within the bound");
    }
    assert_eq!(
        client.open_alert(EVENTS_A, None, &sample_alert()),
        Err(Errno::LimitExceeded)
    );
    loopback.borrow_mut().ticket = TICKET_B;
    assert!(
        client.open_alert(EVENTS_B, None, &sample_alert()).is_ok(),
        "one application's alerts never use up another's"
    );
}

#[test]
fn a_refused_alert_spends_no_id_and_leaves_nothing_owed() {
    let loopback = Loopback::with_regions(&[]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    loopback.borrow_mut().host.refuse_alert = Some(Errno::NotSupported);
    assert_eq!(
        client.open_alert(EVENTS_A, None, &sample_alert()),
        Err(Errno::NotSupported)
    );
    loopback.borrow_mut().host.refuse_alert = None;
    let first = client
        .open_alert(EVENTS_A, None, &sample_alert())
        .expect("accepted once the host can draw it");
    assert_eq!(first, 1, "the refusal minted nothing");
}

#[test]
fn a_withdrawn_alert_comes_down_answered_dismissed() {
    let loopback = Loopback::with_regions(&[]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let alert = client
        .open_alert(EVENTS_A, None, &sample_alert())
        .expect("accepted");
    loopback.borrow_mut().ticket = TICKET_B;
    assert_eq!(client.close_alert(alert), Err(Errno::NotFound));
    loopback.borrow_mut().ticket = TICKET_A;
    client.close_alert(alert).expect("its owner withdraws it");
    let looped = loopback.borrow();
    assert_eq!(looped.host.alerts_withdrawn, [alert]);
    assert_eq!(
        alert_answers(&looped.sink, EVENTS_A),
        [WindowEvent::AlertClosed {
            alert_id: alert,
            outcome: AlertOutcome::Dismissed
        }]
    );
}

#[test]
fn closing_the_window_an_alert_holds_takes_the_alert_down_with_it() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let held = client
        .open_alert(EVENTS_A, Some(window), &sample_alert())
        .expect("accepted");
    let free = client
        .open_alert(EVENTS_A, None, &sample_alert())
        .expect("accepted");
    client.close(window).expect("the owner closes");
    let looped = loopback.borrow();
    assert_eq!(looped.host.alerts_withdrawn, [held]);
    assert_eq!(
        alert_answers(&looped.sink, EVENTS_A),
        [WindowEvent::AlertClosed {
            alert_id: held,
            outcome: AlertOutcome::Dismissed
        }]
    );
    assert_ne!(held, free);
}

#[test]
fn an_exiting_application_takes_its_alerts_down_unanswered() {
    let loopback = Loopback::with_regions(&[]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let alert = client
        .open_alert(EVENTS_A, None, &sample_alert())
        .expect("accepted");
    let owner = proc_id(0xA1);
    let mut looped = loopback.borrow_mut();
    let Loopback { server, host, .. } = &mut *looped;
    server.client_exited(host, owner);
    assert_eq!(host.alerts_withdrawn, [alert]);
    let mut sink = QueueSink::default();
    assert_eq!(
        server.answer_alert(&mut sink, alert, 0, Activation::Withheld),
        Err(Errno::NotFound)
    );
    assert!(sink.delivered.is_empty());
}

#[test]
fn an_answer_naming_a_button_the_alert_lacks_is_never_delivered_as_a_choice() {
    let loopback = Loopback::with_regions(&[]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let alert = client
        .open_alert(EVENTS_A, None, &sample_alert())
        .expect("accepted");
    let buttons = sample_alert().button_count();
    let past = u8::try_from(buttons).expect("a handful");
    let mut sink = QueueSink::default();
    let server = &mut loopback.borrow_mut().server;
    assert_eq!(
        server.answer_alert(&mut sink, alert, past, Activation::Withheld),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        alert_answers(&sink, EVENTS_A),
        [WindowEvent::AlertClosed {
            alert_id: alert,
            outcome: AlertOutcome::Dismissed
        }],
        "its window is down, so it ends unanswered"
    );
    assert_eq!(
        server.answer_alert(&mut sink, alert, 0, Activation::Withheld),
        Err(Errno::NotFound)
    );
}

#[test]
fn answering_an_alert_lets_its_application_bring_the_next_one_forward() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let mine = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    loopback.borrow_mut().ticket = TICKET_B;
    let theirs = create_id(&mut client, 8, EVENTS_B, 1, "b").expect("b");
    loopback.borrow_mut().ticket = TICKET_A;
    loopback.borrow_mut().host.focused = Some(theirs);
    let first = client
        .open_alert(EVENTS_A, Some(mine), &sample_alert())
        .expect("accepted");
    let mut sink = QueueSink::default();
    loopback
        .borrow_mut()
        .server
        .answer_alert(
            &mut sink,
            first,
            0,
            Activation::Granted {
                focus: Some(theirs),
            },
        )
        .expect("answered");
    client
        .open_alert(EVENTS_A, Some(mine), &sample_alert())
        .expect("accepted");
    client
        .open_alert(EVENTS_A, Some(mine), &sample_alert())
        .expect("accepted");
    assert_eq!(
        loopback.borrow().host.alert_focus,
        [false, true, false],
        "the answer earned the front once, for the question after it"
    );
}

#[test]
fn an_alert_takes_the_keyboard_only_when_its_application_has_earned_it() {
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN), (8, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let mine = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    loopback.borrow_mut().ticket = TICKET_B;
    let theirs = create_id(&mut client, 8, EVENTS_B, 1, "b").expect("b");
    loopback.borrow_mut().ticket = TICKET_A;

    // The user is working in this application's window.
    loopback.borrow_mut().host.focused = Some(mine);
    client
        .open_alert(EVENTS_A, Some(mine), &sample_alert())
        .expect("accepted");
    // The user is working in another application's.
    loopback.borrow_mut().host.focused = Some(theirs);
    client
        .open_alert(EVENTS_A, Some(mine), &sample_alert())
        .expect("accepted");
    assert_eq!(loopback.borrow().host.alert_focus, [true, false]);

    // A gesture the user aimed at this application earns it the front once.
    client
        .set_app_bar(&sample_app_bar(EVENTS_A))
        .expect("declared");
    let mut sink = QueueSink::default();
    loopback
        .borrow_mut()
        .server
        .deliver_app_event(
            &mut sink,
            proc_id(0xA1),
            &WindowEvent::AppBarDefault,
            Activation::Granted {
                focus: Some(theirs),
            },
        )
        .expect("delivered");
    client
        .open_alert(EVENTS_A, None, &sample_alert())
        .expect("accepted");
    client
        .open_alert(EVENTS_A, None, &sample_alert())
        .expect("accepted");
    assert_eq!(
        loopback.borrow().host.alert_focus,
        [true, false, true, false],
        "the activation is spent by the alert it earned"
    );
}

#[test]
fn a_refusal_is_an_error_alert_stating_its_reason_as_a_sentence() {
    use crate::client::notice_alert;
    let stated = |reason: &str| {
        let alert =
            notice_alert(AlertSeverity::Error, "No window opened", reason).expect("an alert");
        assert_eq!(alert.severity(), AlertSeverity::Error);
        assert_eq!(alert.headline(), "No window opened");
        let buttons: Vec<_> = alert.buttons().map(AlertButton::role).collect();
        assert_eq!(
            buttons,
            [AlertButtonRole::Default],
            "acknowledged, nothing to decide"
        );
        String::from(alert.detail())
    };
    assert_eq!(stated("permission denied"), "Permission denied.");
    assert_eq!(stated("Already a sentence."), "Already a sentence.");
    assert_eq!(stated("is it?"), "Is it?");
    assert_eq!(
        stated("\u{201c}a.png\u{201d} is not a PNG"),
        "\u{201c}a.png\u{201d} is not a PNG."
    );
    assert_eq!(stated("élan"), "Élan.");
    assert_eq!(stated("cut short\u{2026}"), "Cut short\u{2026}");
    assert_eq!(stated(""), "", "no reason stays none");
}

#[test]
fn a_window_that_could_not_be_opened_is_stated_in_an_alert_holding_none() {
    use crate::client::notice_alert;
    let loopback = Loopback::with_regions(&[]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let refusal = notice_alert(
        AlertSeverity::Error,
        "No window opened",
        "there is not enough memory to draw it",
    )
    .expect("fits");
    let alert = client
        .open_alert(EVENTS_A, None, &refusal)
        .expect("an application with no window is still answered");
    let host = &loopback.borrow().host;
    let (id, _, held, spec) = &host.alerts[0];
    assert_eq!((*id, *held), (alert, None));
    assert_eq!(spec.detail(), "There is not enough memory to draw it.");
}

/// Where a refusal came from, for the notice queue's tests.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum From {
    Listing,
    Operation,
    Paste,
}

/// The headline and detail of the alert `alert_id` the host was asked for.
fn noticed(loopback: &Rc<RefCell<Loopback>>, alert_id: u64) -> (String, String) {
    let looped = loopback.borrow();
    let (_, _, _, spec) = looped
        .host
        .alerts
        .iter()
        .find(|(id, ..)| *id == alert_id)
        .expect("that alert was put up");
    (spec.headline().into(), spec.detail().into())
}

#[test]
fn a_refusal_while_another_is_up_waits_its_turn() {
    use crate::notices::{Notices, Raised};
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let mut notices = Notices::default();
    notices.refuse(From::Listing, "Not listed", "the volume went away");
    assert!(notices.is_showing(), "the window is answering one already");
    assert_eq!(
        notices.raise(&mut client, EVENTS_A, Some(window)),
        Raised::Up
    );
    notices.refuse(From::Operation, "Copy stopped", "the destination is full");
    assert_eq!(
        notices.raise(&mut client, EVENTS_A, Some(window)),
        Raised::Nothing,
        "nothing goes up over the first"
    );
    assert_eq!(
        noticed(&loopback, 1),
        ("Not listed".into(), "The volume went away.".into()),
        "a reason reads as a sentence"
    );
    assert!(!notices.answered(2), "only its own alert answers it");
    assert!(notices.answered(1));
    assert_eq!(
        notices.raise(&mut client, EVENTS_A, Some(window)),
        Raised::Up
    );
    assert_eq!(noticed(&loopback, 2).0, "Copy stopped");
    assert!(notices.answered(2));
    assert!(!notices.is_showing());
}

#[test]
fn a_warning_is_a_warning_alert_waiting_its_turn_like_a_refusal() {
    use crate::notices::{Notices, Raised};
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let mut notices = Notices::default();
    notices.refuse(From::Operation, "Copy stopped", "the destination is full");
    notices.warn(From::Paste, "Copied", "two attributes were not kept");
    assert_eq!(
        notices.raise(&mut client, EVENTS_A, Some(window)),
        Raised::Up
    );
    assert!(notices.answered(1));
    assert_eq!(
        notices.raise(&mut client, EVENTS_A, Some(window)),
        Raised::Up
    );
    let looped = loopback.borrow();
    let (_, _, _, spec) = &looped.host.alerts[1];
    assert_eq!(spec.severity(), AlertSeverity::Warning);
    assert_eq!(
        (spec.headline(), spec.detail()),
        ("Copied", "Two attributes were not kept.")
    );
}

#[test]
fn a_source_keeps_only_its_latest_refusal_waiting() {
    use crate::notices::{Notices, Raised};
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let mut notices = Notices::default();
    notices.refuse(From::Paste, "Not moved", "shown");
    assert_eq!(
        notices.raise(&mut client, EVENTS_A, Some(window)),
        Raised::Up
    );
    for reason in ["stale", "newer", "newest"] {
        notices.refuse(From::Listing, "Not listed", reason);
    }
    notices.refuse(From::Paste, "Not moved", "after");
    assert!(notices.answered(1));
    assert_eq!(
        notices.raise(&mut client, EVENTS_A, Some(window)),
        Raised::Up
    );
    assert_eq!(
        noticed(&loopback, 2).1,
        "Newest.",
        "it keeps its place in line with what it says now"
    );
    assert!(notices.answered(2));
    assert_eq!(
        notices.raise(&mut client, EVENTS_A, Some(window)),
        Raised::Up
    );
    assert_eq!(noticed(&loopback, 3).1, "After.");
    assert!(notices.answered(3));
    assert_eq!(
        notices.raise(&mut client, EVENTS_A, Some(window)),
        Raised::Nothing
    );
}

#[test]
fn a_notice_waits_while_its_application_has_every_alert_it_may() {
    use crate::notices::{Notices, Raised};
    use tairix_abi::window_ipc::ALERT_MAX_OPEN;
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    for _ in 0..ALERT_MAX_OPEN {
        client
            .open_alert(EVENTS_A, None, &sample_alert())
            .expect("within the bound");
    }
    let mut notices = Notices::default();
    notices.refuse(From::Listing, "Not listed", "the volume went away");
    assert_eq!(
        notices.raise(&mut client, EVENTS_A, Some(window)),
        Raised::Full
    );
    assert!(notices.is_showing(), "it is kept, not lost");
    let mut sink = QueueSink::default();
    loopback
        .borrow_mut()
        .server
        .answer_alert(&mut sink, 1, 0, Activation::Withheld)
        .expect("answered");
    assert_eq!(
        notices.raise(&mut client, EVENTS_A, Some(window)),
        Raised::Up
    );
}

#[test]
fn a_notice_the_desktop_will_not_show_is_handed_back_to_be_stated() {
    use crate::notices::{Notices, Raised};
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let window = create_id(&mut client, 7, EVENTS_A, 1, "a").expect("a");
    let mut notices = Notices::default();
    notices.refuse(From::Listing, "Not listed", "the volume went away");
    loopback.borrow_mut().host.refuse_alert = Some(Errno::NotSupported);
    let Raised::Refused(Errno::NotSupported, notice) =
        notices.raise(&mut client, EVENTS_A, Some(window))
    else {
        panic!("refused");
    };
    assert_eq!(notice.headline(), "Not listed");
    assert!(!notices.is_showing(), "dropped once handed back");
}

/// A refusal no window asked for — an open that failed after the window had
/// moved on — goes up holding none.
#[test]
fn a_notice_no_window_asked_for_goes_up_holding_none() {
    use crate::notices::{Notices, Raised};
    let loopback = Loopback::with_regions(&[(7, FRAME_LEN)]);
    let mut client = WindowClient::new(Rc::clone(&loopback));
    let mut notices = Notices::default();
    notices.refuse(From::Listing, "Not opened", "the volume went away");
    assert_eq!(notices.raise(&mut client, EVENTS_A, None), Raised::Up);
    let looped = loopback.borrow();
    let (_, _, owner, _) = looped.host.alerts.last().expect("it went up");
    assert_eq!(*owner, None, "it holds no window");
}

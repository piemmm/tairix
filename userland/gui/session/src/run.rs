//! The `Run` entry-point binary of the desktop session component, installed
//! as a signed bundle in the component store (`/System/Components/`) and
//! started by a graphical login (`os.loginType graphical`) as the
//! authenticated user's session (`plans/DISPLAY.md` D7c). The reserved
//! `-h`/`-?` switches serve its own short help; the grammar is otherwise
//! closed (see [`tairix_desktop_session::cli`]).
//!
//! This is the client half of the zero-copy, lease-gated present path: the
//! session acquires the boot seat's exclusive, revocable lease, brings the
//! display client up over the reserved `DISPLAY_ENDPOINT` (query the mode →
//! create the shared double-buffered frame region → grant it to the display
//! service → configure), and then runs the desktop from its wait-set: it
//! parks on a `SeatInput` member (woken by input delivery *and* by lease
//! loss), drains the owned seat's pointer and keyboard channels through the
//! session crate's fail-closed record path, pumps each decoded event
//! through the `DesktopShell`, and presents the composited damage by
//! frame index — no frame bytes ever cross the IPC.
//!
//! It is a **pure-Rust** program: TAIRiX is Rust-only, so it links the Rust
//! userland runtime `tairix-rt`, never the C ABI (which exists solely for
//! non-Rust programs). `tairix-rt` provides `_start`, the per-process stack
//! canary, the panic handler, the allocator, and the syscall wrappers;
//! `tairix_rt::entry!` names this program's `main`.
//!
//! `main` wires the real seams the shared engines drive:
//!
//! * `display_acquire(SEAT_PRIMARY)`: the kernel binds this task as the
//!   seat's owner and mints the revocable lease. Every later drain and
//!   present is owner-gated kernel-side against that live lease — the
//!   session holds no oracle and asserts nothing.
//! * `DisplayClient` over `ipc_call` to the reserved `DISPLAY_ENDPOINT`:
//!   the display service re-checks the caller's live lease per request via
//!   `call_peer_seat`, so a stale session cannot scribble on a switched
//!   seat.
//! * `shm_create` + `shm_grant`: the frame region is the session's own
//!   kernel-zeroed mapping, granted *to the serving task of the display
//!   endpoint* — never to a raw, recyclable PID.
//! * `SeatEventReader` over the seat-addressed `pointer_read` /
//!   `keyboard_read`: each drain is `CAP_INPUT_READ`- and owner-gated
//!   kernel-side; a truncated or malformed record fails closed in the
//!   session crate's one validation path, never decoding as a spurious
//!   event.
//! * The `SeatInput` wait-set member: the session parks between events —
//!   never a poll loop — and is woken by input *or* by losing the seat, so
//!   a revoked session observes the typed refusal on its very next drain
//!   and tears down fail-loud instead of parking forever or repainting
//!   blind.
//!
//! The session also binds the three seat-scoped rendezvous — the window
//! channel, the notification channel, and the Switchboard tray-summary
//! channel — and spawns the desktop's Switchboard monitor service as the
//! logged-in user at bring-up. The monitor's change-driven summaries feed
//! the taskbar capsule (each publish attested against the launch table);
//! the session's own delivery evidence (the `HangTracker` behind the event
//! sink) feeds the capsule's "not responding" count; and a monitor that
//! dies or was never there simply leaves the capsule calm
//! (`plans/NEW-TASKBAR.md` T9/T10).
//!
//! Loss of the seat (`SeatRevoked` / `SeatNotOwner` on any drain or
//! present) ends the session with its reason on `stderr` and a reserved
//! exit code; the spawning supervisor decides whether a fresh session (with
//! a fresh acquire and a fresh configure) replaces it. Every other fault —
//! a dead display service, a refused wait-set, a malformed input record —
//! is equally fail-loud: the session never spins, never guesses a mode, and
//! never repaints without a live lease.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy,
//! and fmt still cover the file.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

// --- Pure-Rust program --------------------------------------------------
#[cfg(freestanding)]
mod program {
    use alloc::collections::BTreeMap;
    use alloc::vec::Vec;

    use tairix_abi::driver::audio::StreamDirection;
    use tairix_abi::driver::display::{DisplayMode, OutputMode};
    use tairix_abi::elevate::{
        elevate_endpoint, ElevateArgv, ElevateReply, ElevateRequest, WiredLaunch, ELEVATE_MAX_REPLY,
    };
    use tairix_abi::input::{KeyInput, Modifiers as AbiModifiers};
    use tairix_abi::ipc::MemoryPause;
    use tairix_abi::latency::DEFAULT_FRAME_BUDGET_NS;
    use tairix_abi::notify_ipc::{NotifyRequest, NOTIFY_ENDPOINT, NOTIFY_MAX_REQUEST};
    use tairix_abi::pinboard_ipc::{
        encode_sound_reading, encode_sound_themes, PINBOARD_ENDPOINT, PINBOARD_MAX_REPLY,
        PINBOARD_MAX_REQUEST, SOUND_THEMES_PAGE,
    };
    use tairix_abi::reply::{encode_status_reply, STATUS_REPLY_LEN};
    use tairix_abi::seat::ReleaseSurface;
    use tairix_abi::seat::SEAT_PRIMARY;
    use tairix_abi::secret::Secret;
    use tairix_abi::session_ipc::decode_account_page;
    use tairix_abi::session_ipc::{
        session_wake_endpoint, SessionRequest, SessionVerdict, SessionWake, SESSION_ENDPOINT,
        SESSION_MAX_REQUEST, SESSION_VERDICT_LEN, SESSION_WAKE_LEN,
    };
    use tairix_abi::sound_cue::SoundEvent;
    use tairix_abi::switchboard_ipc::{
        command_endpoint_for, CommandSection, SwitchboardCommand, SEAT_REPORT_OWNERS_MAX,
        SWITCHBOARD_ENDPOINT, SWITCHBOARD_MAX_REQUEST, SWITCHBOARD_PUBLISH_REPLY_LEN,
    };
    use tairix_abi::time::WallTimeState;
    use tairix_abi::window_ipc::LandingStep;
    use tairix_abi::window_ipc::{
        event_endpoint_for, AlertSeverity, AlertSpec, AuthorisedOutcome, BundleRunPath,
        DocumentName, DragItems, DropTarget, MenuOutcome, PointerAction, PreviewOutcome,
        WindowEvent, WINDOW_ENDPOINT, WINDOW_MAX_REQUEST,
    };
    use tairix_abi::{
        CapabilityId, Errno, FdWire, Notice, NoticeTopic, Origin, ProcId, WaitFlags, WaitSetOp,
        WaitSourceKind, WaitStatus, DATETIME_RUN_PATH, ENV_SHOWN_NAME, FILES_RUN_PATH,
        ORIGIN_WIRE_LEN, SETTINGS_RUN_PATH, STDIN, WAITSET_TIMEOUT_NONE, WAIT_PID_ANY,
    };
    use tairix_appdata::RtHost;
    use tairix_audio::live::{live_captures, RtAudio};
    use tairix_audio::stream::{devices, set_control, DeviceControl};
    use tairix_browse::follow::{
        BundleFollows, FollowParts, FollowWork, FollowedBundle, Follower, Unfollowed,
    };
    use tairix_browse::lanes::{Crew, Decodes, Held, Lane, LaneSignal, LaneWork, Routes, Serving};
    use tairix_browse::{
        AppAssociation, DirectorySource, Entry, FilerPreferences, Listing, ListingDesk, ListingJob,
        Probe, Probes, RtLinkReader, Took, WatchUpdate, WatchedDirectory, Watches,
        WATCH_BUFFER_LEN,
    };
    use tairix_caps::CapabilitySet;
    use tairix_controls::{damage, RowIcon};
    use tairix_desktop_session::about::SystemFacts;
    use tairix_desktop_session::alert::{alert_title, Speaker};
    use tairix_desktop_session::capture::{
        claim_recording, keep_still, log_kept, settle_recording, sweep_captures, take_output,
        Allowed, CaptureController, CaptureNotice, CaptureWork, Kept, KeptFolder, RecordPlan,
        RecordStep, RecorderHost, Recording, RecordingFile, Screened, StillKept, StillShot,
        StopReason,
    };
    use tairix_desktop_session::desktop::{LandingOffer, LandingWork};
    use tairix_desktop_session::drag::PlaceRefused;
    use tairix_desktop_session::effects::{
        load_choices, store_choices, Effects, EffectsWork, LoadedChoices, ReadAsk, MAX_HELD_CALLS,
    };
    use tairix_desktop_session::held::{
        HeldDone, HeldJob, HeldQuestions, HeldSystem, HeldWord, HELD_JOBS,
    };
    use tairix_desktop_session::keep::{keep_picture, kept_at, Found, KeptFiles, Picture, Unkept};
    use tairix_desktop_session::launcher::{
        elevated_launch, launched_entries, load_personal, running_bundles, store_personal,
        LauncherMemory, LoadedPersonal,
    };
    use tairix_desktop_session::menu::{open_desktop_menu, ChainOutcome, ChainOwner, MenuChain};
    use tairix_desktop_session::output::{Follow, Refusal};
    use tairix_desktop_session::picker::{PickId, PickSources};
    use tairix_desktop_session::pinboard::{
        load_arrangement, ArrangementMemory, LoadedArrangement, PinboardCommand,
    };
    use tairix_desktop_session::saver::{load_savers, SaverCatalog, SaverErrands, SaverMemory};
    use tairix_desktop_session::sound::{SoundAnswer, SoundJob, SoundSession};
    use tairix_desktop_session::switchuser::{
        SeatDeparture, SeatReturn, SessionAuthority, SwitchUser,
    };
    use tairix_desktop_session::windows::window_menu_placement;
    use tairix_desktop_session::TapKey;
    use tairix_desktop_session::{
        admitted_pid, adopt_unlocks, catalogued, chain_geometry, deliver_pending_open,
        desktop_info, drain_authorising, drain_away, drain_locked, drop_is_noteworthy,
        encode_switchboard_reply, focus_reports, hand_file_manager, land_preview, launch_argv,
        load_pinboard as read_pinboard_store, load_programs, maybe_send_seat_report, open_entry,
        open_tray, park_within, parse, publish_asked, reap_launched, relay_power,
        resize_drag_event, resolve_launch, resolve_window_identities, seat_held, serve_park_ns,
        serve_pinboard, serve_switchboard_request, settled_size_event, size_state_name,
        tool_move_abandoned_event, tool_move_event, trusted_up, window_control_alternate_event,
        window_control_event, Acquisition, AidPolicy, Answer, AppBarBridge, AppBarService,
        AppearanceWork, ArtworkFileReader, AskedSettings, AuthoriseAnswer, AuthoriseAsker,
        AuthoriseDesk, AuthoriseDone, AuthorisePrompt, AuthoriseRefusal, AuthoriseRequest,
        BundleIndex, CliError, Command, ConfirmPrompt, Delivery, Departure, Desktop, DesktopAction,
        DesktopActivation, DesktopDrop, DesktopEnd, DesktopOutcome, DesktopShell,
        DeviceInputSource, DocumentAuthority, DocumentRelay, DragEnd, DragPlace, DragSurface,
        DropRefusal, ElevateAsk, Elevation, Elevator, FileManagerFound, FileManagerHand,
        FileManagerKeeper, FileOffer, FrameContent, FramePacer, FrameReportGate, FrameShow,
        FrameStatsPublisher, FrameStatsSink, GestureRefusal, HangTracker, HeldTarget, HoldBack,
        IdleAction, IdleClock, IdlePolicy, InputPolicy, ItemsOffer, ItemsOfferEnd,
        KeyboardInputSource, Launch, LaunchDocument, LaunchHost, LaunchTable, LaunchTarget,
        LayerDecision, LayerFeed, Listing as Authorisers, LoadedPrograms, MachineWatch, OfferEnd,
        OwedReport, OwnPick, OwnerBundleGate, OwnerWindow, PickAccess, PickEnd, PickStep,
        PinboardApplied, PinboardCall, PinboardRefusal, PlacedItem, Prepared, PresentedOwners,
        PreviewBudget, PreviewDone, PreviewJob, PreviewRequest, PreviewRun, PreviewTarget,
        PromptOutcome, Publication, Requester, ResizedRecord, ResolutionAsk, ResolutionPrompt,
        Routed, SaveEnd, SaveOffer, SaverContext, SaverEnd, SaverLaunch, SaverLauncher, SaverSetup,
        ScreenFade, ScreenLock, Screensaver, Seat, SeatDrain, SeatEventReader, SeatInputChannel,
        SeatRouter, SeatWake, SessionFileReader, SessionPicker, SessionWindows, ShellWindowHost,
        SizedRecord, SlideWant, SlotAnswer, SoundCall, SwitchboardMailbox, SwitchboardOutcome,
        SwitchboardServe, Timed, Trial, TrialEnd, TrialOutcome, Typing, UnlockAnswer, UnlockAsk,
        WallpaperDesk, WallpaperJob, WallpaperService, WallpaperSource, WindowUnder, APP_ATTACH,
        APP_BAR_SETTLED, APP_BAR_SETTLED_MESSAGE, APP_BAR_SLOT_SHOWN, APP_BAR_SLOT_SHOWN_MESSAGE,
        CONTENT_RELEASED, CONTENT_RELEASED_MESSAGE, DESKTOP_RESIZED, DESKTOP_RESIZED_MESSAGE,
        DESKTOP_RESTYLED, DESKTOP_RESTYLED_MESSAGE, ELEVATE_PROMPT_SHOWN,
        ELEVATE_PROMPT_SHOWN_MESSAGE, FILES_LABEL, LAYER_FEEDS, LAYER_FEEDS_RESUMED_MESSAGE,
        LAYER_FEEDS_STOPPED_MESSAGE, LAYER_OPENED, LAYER_OPENED_MESSAGE, LAYER_REFUSED,
        LAYER_REFUSED_MESSAGE, LAYER_RETIRED, LAYER_RETIRED_MESSAGE, LIBRARY_SHOWN,
        LIBRARY_SHOWN_MESSAGE, MENU_SHOWN, MENU_SHOWN_MESSAGE, MIN_FRAME_PUBLISH_INTERVAL_NS,
        PICKER_SHOWN, PICKER_SHOWN_MESSAGE, PINBOARD_SETTLED, PINBOARD_SETTLED_MESSAGE,
        SETTINGS_LABEL, SET_TIME_PURPOSE, SWITCHBOARD_CALL_REFUSED, SWITCHBOARD_LABEL,
        SWITCHBOARD_RUN_PATH, TRIAL_CONCLUDED, TRIAL_CONCLUDED_MESSAGE, TRIAL_SHOWN,
        TRIAL_SHOWN_MESSAGE, USAGE, WINDOW_RETITLED, WINDOW_RETITLED_MESSAGE, WINDOW_SHOWN,
        WINDOW_SHOWN_MESSAGE, WINDOW_SIZED, WINDOW_SIZED_MESSAGE,
    };
    use tairix_desktop_session::{machine_zone_place, moves_machine_zone, read_machine_zone};
    use tairix_desktop_session::{
        preview_source, resolution_ask, slide_positions, slide_source, CivilAdoption, HeldZones,
        ScreensaverPreview, ScreensaverServe,
    };
    use tairix_display::{
        DisplayClient, FramesRefused, Presenter, RtDisplay, RtDisplayTransport, RtShmMapper,
        SwitchedOff,
    };
    use tairix_greeter::AccountTile;
    use tairix_greeter::{Asking, Courier, Ticket, Tickets, Verdict};
    use tairix_icon::{
        ArtworkDesk, ArtworkJob, ArtworkKey, ArtworkRasteriser, ArtworkResolver, BundleVersion,
        InlineArtwork, Resolved,
    };
    use tairix_input::wire::modifiers_to_abi;
    use tairix_log::{
        log, Event as LogEvent, EventId, Field as LogField, FieldValue as LogFieldValue,
        Level as LogLevel,
    };
    use tairix_parallel::Pool;
    use tairix_procinfo::IpcTransport;
    use tairix_proglib::Edit;
    use tairix_rt::io;
    use tairix_rt::work::Worker;
    use tairix_rt::ServedCall;
    use tairix_sandbox::imagerender::{
        plan_wallpaper, rasterise_icon, upload_document, ImageRenderService, UploadFailure,
        WallpaperRenderFailure, WALLPAPER_PREPARATION_BYTES,
    };
    use tairix_sandbox::rt::{
        serve_stdio, worker_role, RtLauncher, RtSessionChannel, SessionMembers,
    };
    use tairix_sandbox::ParserSandbox;
    use tairix_soundtheme::{SoundSettings, UserChoices};
    use tairix_taskbar::{
        MenuRequest, MenuSubject, SoundAction, TaskId, TaskbarConfig, TaskbarResponse,
    };
    use tairix_theme::Accessibility;
    use tairix_tz::{Decided, ZoneInfo};
    use tairix_wallpaper::{
        merge_applied, DesktopSettings, Resolution, ScreensaverChoice, MAX_WALLPAPER_BYTES,
        WALLPAPER_STORE,
    };
    use tairix_window::{
        app, Activation, AuthoriseFor, CallerIdentity, ClientRegion, DragConclusion, DragReport,
        EventSink, OpenEntry, PickedFile, WallpaperName, WindowServer, WINDOW_REPLY_MAX,
    };
    use tairix_wm::{
        chrome_cache, frost_cache, Compositor, InputResponse, Point, Presentation, Rect, Region,
        Surface, WindowControlKind,
    };

    extern crate alloc;

    /// Exit code when the boot seat's lease could not be acquired (held by
    /// another session, or the manifest lacks `CAP_DISPLAY`). A reserved,
    /// fail-closed value.
    const EXIT_NO_SEAT: i32 = 90;

    /// Exit code when the display service could not be reached or refused
    /// the bring-up handshake (no bound endpoint, a refused query, a
    /// refused configure). A reserved, fail-closed value: the session never
    /// renders against a guessed mode.
    const EXIT_NO_DISPLAY: i32 = 91;

    /// Exit code when the queried mode is unusable (zero-sized, or its
    /// frame arithmetic overflows the address width). A reserved,
    /// fail-closed value.
    const EXIT_BAD_MODE: i32 = 92;

    /// Exit code when the shared frame region could not be created or
    /// granted to the display service. A reserved, fail-closed value.
    const EXIT_NO_FRAMES: i32 = 93;

    /// Exit code when the wait-set the session parks on could not be
    /// created, populated, or waited on. A reserved, fail-closed value: the
    /// session exits rather than degrade into a busy re-poll.
    const EXIT_WAIT_FAILED: i32 = 94;

    /// Exit code when the seat lease was lost (revoked by the seat manager
    /// or released from under the session): the typed `SeatRevoked` /
    /// `SeatNotOwner` observed on a drain or present. The session's normal
    /// fail-loud teardown, never an error in the session itself.
    const EXIT_SEAT_LOST: i32 = 95;

    /// Exit code when a seat input drain faulted for a reason other than
    /// losing the lease (a malformed record surfaced by the fail-closed
    /// decode path). A reserved value: an untrustworthy input stream ends
    /// the session rather than being skipped over.
    const EXIT_INPUT_FAULT: i32 = 96;

    /// Exit code when a present was refused for a reason other than losing
    /// the lease (a dead display service, a device fault). A reserved,
    /// fail-closed value.
    const EXIT_PRESENT_FAILED: i32 = 97;

    /// Exit code when the reserved `WINDOW_ENDPOINT` could not be bound.
    /// The kernel authorises the bind by this session's live seat lease
    /// (no privileged-bind capability), so a refusal means the lease is
    /// gone or another server already claimed the rendezvous — exit
    /// fail-loud, never serve a desktop apps cannot reach.
    const EXIT_NO_WINDOW_ENDPOINT: i32 = 98;

    /// Exit code when the reserved `NOTIFY_ENDPOINT` could not be bound. It
    /// is another seat-scoped reserved id, authorised by the same live
    /// seat lease as the window endpoint, so a refusal here is the same
    /// lease/rendezvous anomaly — exit fail-loud rather than run a desktop
    /// whose services cannot post notifications.
    const EXIT_NO_NOTIFY_ENDPOINT: i32 = 99;

    /// Exit code when the reserved `SWITCHBOARD_ENDPOINT` could not be
    /// bound. The third seat-scoped reserved id, authorised by the same
    /// live seat lease — the same lease/rendezvous anomaly as the other
    /// two, so the session exits fail-loud rather than run a desktop whose
    /// monitor cannot publish.
    const EXIT_NO_SWITCHBOARD_ENDPOINT: i32 = 100;

    /// Exit code when the reserved `PINBOARD_ENDPOINT` could not be bound.
    /// It is authorised by this session's kernel-attested live seat lease —
    /// the same lease/rendezvous anomaly as the other three — so the session
    /// exits fail-loud rather than run a desktop whose settings surfaces
    /// can never apply anything.
    ///
    /// Out of sequence with its neighbours because the slot it would have
    /// taken is [`tairix_rt::EXIT_PANIC`], and a session that exits with the
    /// runtime's panic status cannot be told from one that panicked.
    const EXIT_NO_PINBOARD_ENDPOINT: i32 = 104;

    /// Exit code when the user chose *Log Out*. The session ended because it
    /// was asked to, so it is a success: nothing failed, and the login
    /// supervisor that started the desktop prompts again.
    const EXIT_LOGGED_OUT: i32 = 0;

    /// Exit code when a resumed session could not take its screen back: the
    /// seat, the mode, the frame region, or the compositor's adoption of the
    /// new mode refused. Reserved, so the supervisor can tell a desktop that
    /// came back blind from one that was logged out of.
    const EXIT_RESUME_FAILED: i32 = 102;

    /// Exit code when the session authority went away while this session was
    /// parked in the background: nothing can resume it, so it ends cleanly
    /// rather than being stranded invisible.
    const EXIT_AUTHORITY_GONE: i32 = 103;

    /// The wait-set token of the session's `SeatInput` member.
    const SEAT_TOKEN: u64 = 1;

    /// The wait-set token of the port the display driver posts its latches
    /// to.
    const DISPLAY_EVENT_TOKEN: u64 = 25;

    /// The wait-set token of the served `WINDOW_ENDPOINT` member.
    const WINDOW_TOKEN: u64 = 2;

    /// The wait-set token of the any-child member: a spawned app exiting
    /// wakes the loop so its windows are torn down promptly.
    const CHILD_TOKEN: u64 = 3;

    /// The wait-set token of the served `NOTIFY_ENDPOINT` member: a producer
    /// posting or clearing a notification wakes the loop to relay it.
    const NOTIFY_TOKEN: u64 = 4;

    /// The wait-set token of the served `SWITCHBOARD_ENDPOINT` member: the
    /// Switchboard service publishing a tray summary wakes the loop to
    /// relay it to the capsule.
    const SWITCHBOARD_TOKEN: u64 = 5;

    /// The wait-set token of the memory-pressure member: the kernel wakes
    /// the loop when the machine's pressure band changes, so the desktop
    /// gives its cached pixels back as memory tightens instead of holding
    /// them until something else is starved.
    const PRESSURE_TOKEN: u64 = 6;

    /// The wait-set token of the served `PINBOARD_ENDPOINT` member: a tool
    /// the user ran (the Settings application) asking the session to adopt
    /// new pinboard settings wakes the loop to apply them.
    const PINBOARD_TOKEN: u64 = 7;

    /// The wait-set token every held-back destination's room member carries:
    /// an app draining its full event mailbox wakes the loop to send it what
    /// it is owed. One token for all of them — the flush offers every
    /// destination its events anyway, so which one drained is not worth
    /// distinguishing.
    const HOLDBACK_TOKEN: u64 = 8;

    /// The wait-set token of this session's fast-user-switching wake
    /// mailbox: the session authority telling a background desktop it is the
    /// foreground one again, or that it must end.
    const WAKE_TOKEN: u64 = 9;

    /// The wait-set token of the session's worker wake pipe: a directory read or
    /// a wallpaper preparation the session asked for has finished, so whichever
    /// consumer was waiting can adopt it and repaint.
    const WORKER_TOKEN: u64 = 10;

    /// The wait-set token of the pinboard's folder watch: a program changed
    /// the user's `Desktop` folder, so the worker drains what it did.
    const PINBOARD_WATCH_TOKEN: u64 = 11;

    /// The wait-set token of the sound worker's wake: a round trip to the
    /// audio service has come back.
    const SOUND_TOKEN: u64 = 13;

    /// The wait-set token of the `AudioDevices` notice: a device or one of
    /// its controls moved, whoever moved it.
    const AUDIO_DEVICES_TOKEN: u64 = 14;

    /// The wait-set token of the `AudioCapture` notice the recording
    /// indicator is drawn from.
    const AUDIO_CAPTURE_TOKEN: u64 = 15;

    /// The wait-set token of the sound authority's answer to a notification's
    /// arrival sound.
    const CUE_TOKEN: u64 = 16;

    /// The wait-set token of the `DisplayOutput` notice: the output the
    /// session draws on moved (`plans/DISPLAY.md` D12).
    const DISPLAY_OUTPUT_TOKEN: u64 = 17;

    /// The wait-set token of the private-store worker's wake: a round trip
    /// for the user's sound choices or the launcher's record has come back.
    const PRIVATE_TOKEN: u64 = 18;

    /// The wait-set token of the `CueAuthority` notice: the cue authority
    /// started, and holds none of this session's choices.
    const CUE_AUTHORITY_TOKEN: u64 = 19;

    /// The wait-set token of the unlock worker's wake: the broker answered a
    /// password offered at the screen lock.
    const UNLOCK_TOKEN: u64 = 20;

    /// The wait-set token of the elevation worker's wake: the broker answered
    /// a question the authorisation prompt asked.
    const ELEVATE_TOKEN: u64 = 21;

    /// The wait-set token of the watch on the directory the machine's zone
    /// file is kept in: something there changed.
    const MACHINE_ZONE_WATCH_TOKEN: u64 = 22;

    /// The wait-set token of the machine zone worker's wake: it drained the
    /// watch, and read the file where the file moved.
    const MACHINE_ZONE_TOKEN: u64 = 23;

    /// The wait-set token of the font worker's wake: text the desktop drew
    /// provisionally has been fetched.
    const FONT_TOKEN: u64 = 24;

    /// The wait-set token of a hosted saver's pipe: it said something.
    const SAVER_READ_TOKEN: u64 = 26;

    /// The wait-set token of room in a hosted saver's pipe for what the
    /// session queued for it.
    const SAVER_WRITE_TOKEN: u64 = 27;

    /// The wait-set token of the saver font relay's wake: the font service
    /// answered a call a saver made.
    const SAVER_FONT_TOKEN: u64 = 28;

    /// The wait-set token of the saver file worker's wake: a picture a saver
    /// handed over was kept, or the screensaver stores were read.
    const SAVER_FILE_TOKEN: u64 = 29;

    /// The wait-set token of the watch on the user's own screensaver store: a
    /// saver was put there, taken away or renamed.
    const SAVER_STORE_TOKEN: u64 = 30;

    /// The wait-set token of every child taken down while it still ran — a
    /// saver or the recorder: ready once one has exited, to be reaped.
    const REAPED_TOKEN: u64 = 31;

    /// The wait-set token of the `Mounts` notice: a volume came, went or came
    /// back, so a desktop folder no watch could follow may be followed now.
    const MOUNTS_TOKEN: u64 = 32;

    /// The first wait-set token a pick's folder watch reports under: pick
    /// `n`'s is this plus `n`.
    const PICKER_WATCH_BASE: u64 = 1 << 32;

    /// Every fixed wait-set token, which a pick's watch token stays clear of.
    const FIXED_TOKENS: [u64; 31] = [
        SEAT_TOKEN,
        WINDOW_TOKEN,
        CHILD_TOKEN,
        NOTIFY_TOKEN,
        SWITCHBOARD_TOKEN,
        PRESSURE_TOKEN,
        PINBOARD_TOKEN,
        HOLDBACK_TOKEN,
        WAKE_TOKEN,
        WORKER_TOKEN,
        PINBOARD_WATCH_TOKEN,
        SOUND_TOKEN,
        AUDIO_DEVICES_TOKEN,
        AUDIO_CAPTURE_TOKEN,
        CUE_TOKEN,
        DISPLAY_OUTPUT_TOKEN,
        PRIVATE_TOKEN,
        CUE_AUTHORITY_TOKEN,
        UNLOCK_TOKEN,
        ELEVATE_TOKEN,
        MACHINE_ZONE_WATCH_TOKEN,
        MACHINE_ZONE_TOKEN,
        FONT_TOKEN,
        DISPLAY_EVENT_TOKEN,
        SAVER_READ_TOKEN,
        SAVER_WRITE_TOKEN,
        SAVER_FONT_TOKEN,
        SAVER_FILE_TOKEN,
        SAVER_STORE_TOKEN,
        REAPED_TOKEN,
        MOUNTS_TOKEN,
    ];

    const _: () = {
        let mut at = 0;
        while at < FIXED_TOKENS.len() {
            assert!(FIXED_TOKENS[at] < PICKER_WATCH_BASE);
            at += 1;
        }
    };

    /// The wait-set tokens of the recorder's reply pipe and of room in its
    /// request pipe.
    const RECORDER_READ_TOKEN: u64 = 33;
    const RECORDER_WRITE_TOKEN: u64 = 34;

    /// The wait-set token of the capture file worker's wake: a capture was
    /// kept, or a recording's file made or named.
    const CAPTURE_FILE_TOKEN: u64 = 35;

    /// The wait-set token of the held-writes worker's wake: it read which
    /// volumes await the seat's answer, or the kernel took an answer.
    const HELD_WRITES_TOKEN: u64 = 48;

    /// The key the desktop's own capture notices go by, so each replaces the
    /// last rather than stacking.
    const CAPTURE_NOTICE_KEY: u32 = 1;

    /// Whether the member `token` names reports once, the wait consuming its
    /// report as it delivers it — a directory watch or a system notice —
    /// rather than reporting until it is drained.
    const fn reports_once(token: u64) -> bool {
        matches!(
            token,
            PINBOARD_WATCH_TOKEN
                | MACHINE_ZONE_WATCH_TOKEN
                | SAVER_STORE_TOKEN
                | PRESSURE_TOKEN
                | AUDIO_DEVICES_TOKEN
                | AUDIO_CAPTURE_TOKEN
                | DISPLAY_OUTPUT_TOKEN
                | CUE_AUTHORITY_TOKEN
                | MOUNTS_TOKEN
        ) || token >= PICKER_WATCH_BASE
    }

    /// How often the user's screensaver store's watch may report: a bundle
    /// copied in costs one read of the stores, and is most often whole by
    /// the time it is read.
    const SAVER_STORE_WATCH_LATENCY_NS: u64 = 1_000_000_000;

    /// The most change batches one read of the stores drains from the user
    /// store's watch before it reads them, so a store changing without pause
    /// cannot hold the worker.
    const SAVER_STORE_DRAIN_BATCHES: usize = 16;

    /// How often the configuration directory's watch may report. A changed
    /// zone shows within this, and a burst of writes costs one read.
    const MACHINE_ZONE_WATCH_LATENCY_NS: u64 = 250_000_000;

    /// The thread the machine zone's watch is drained and the configuration
    /// read on, so neither waits on the volume on the serve loop. It holds the
    /// watched directory; an answer is `None` where the drain named no change
    /// to the configuration.
    type ZoneWorker = Worker<
        Option<tairix_rt::Dir>,
        (),
        Option<Result<Option<tairix_abi::zone::ZoneName>, alloc::string::String>>,
    >;

    /// The machine zone worker's job: drain the watch, and read the
    /// configuration where the drain says it moved.
    fn drain_machine_zone(
        dir: &mut Option<tairix_rt::Dir>,
        (): &mut (),
    ) -> Option<Result<Option<tairix_abi::zone::ZoneName>, alloc::string::String>> {
        let dir = dir.as_ref()?;
        let mut scratch = [0u8; 2 * tairix_abi::fs::DirChangeBatch::MIN_BUFFER];
        let mut moved = false;
        let mut drained = false;
        for _ in 0..MACHINE_ZONE_DRAIN_BATCHES {
            let Ok(len) = dir.read_changes(&mut scratch) else {
                moved = true;
                drained = true;
                break;
            };
            let (named, more) = moves_machine_zone(scratch.get(..len).unwrap_or_default());
            moved |= named;
            if !more {
                drained = true;
                break;
            }
        }
        (moved || !drained).then(|| read_machine_zone(&mut VfsFileReader))
    }

    /// The most change batches one drain reads before it re-reads the
    /// configuration rather than read on, so a directory changing without
    /// pause cannot hold the worker.
    const MACHINE_ZONE_DRAIN_BATCHES: usize = 16;

    /// The directory the machine configuration is kept in, watched and joined
    /// to `set`, so a change to the machine's zone made while the desktop runs
    /// shows at once; `None`, stated, where it cannot be.
    fn watch_machine_settings(set: u64) -> Option<tairix_rt::Dir> {
        let (place, _) = machine_zone_place();
        let watched = tairix_rt::Dir::open(place.as_bytes())
            .and_then(|dir| dir.watch(MACHINE_ZONE_WATCH_LATENCY_NS).map(|()| dir))
            .map_err(Errno::from_syscall)
            .and_then(|dir| {
                let joined = tairix_rt::waitset_ctl(
                    set,
                    WaitSetOp::Add,
                    WaitSourceKind::DirWatch,
                    u64::from(dir.fd()),
                    MACHINE_ZONE_WATCH_TOKEN,
                );
                if joined == 0 {
                    Ok(dir)
                } else {
                    Err(Errno::from_syscall(joined))
                }
            });
        watched
            .map_err(|err| {
                app::report(
                    APP_NAME,
                    format_args!(
                        "the clock will not follow a change to the machine's time zone ({err})"
                    ),
                );
            })
            .ok()
    }

    /// Hold the machine zone `read` named, stating a refusal and keeping the
    /// zone held before it; answers whether the zone moved.
    fn hold_machine_zone(
        read: Result<Option<tairix_abi::zone::ZoneName>, alloc::string::String>,
        shell: &mut DesktopShell,
    ) -> bool {
        match read {
            Ok(zone) => {
                if let Some(name) = zone.filter(|name| {
                    shell
                        .zones()
                        .is_some_and(|zones| zones.zone(name.as_str()).is_none())
                }) {
                    app::report(
                        APP_NAME,
                        format_args!(
                            "the machine's zone {} is not in the zone store; the clock reads its \
                             default",
                            name.as_str()
                        ),
                    );
                }
                shell.set_machine_zone(zone)
            }
            Err(reason) => {
                app::report(APP_NAME, format_args!("{reason}; the clock keeps its zone"));
                false
            }
        }
    }

    /// Adopt a re-read of the machine's zone: the clock reads it for a user
    /// who chose none, and every application is told it.
    fn adopt_machine_zone(
        read: Result<Option<tairix_abi::zone::ZoneName>, alloc::string::String>,
        settings: &DesktopSettings,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        if !hold_machine_zone(read, shell) {
            return;
        }
        let reshaped = shell.adopt_civil(settings).reshaped;
        if !tick_clock(shell, compositor, tairix_rt::clock_get()) && reshaped {
            shell.present(compositor);
        }
        publish_desktop(compositor, shell);
    }

    /// The display the session presents through: the frames its driver
    /// allocated, mapped by the ring and unmapped with it, and the port the
    /// driver posts each latch to.
    type Ring = RtDisplay<RtDisplayTransport>;

    /// Wakes one sender may have queued at the mailbox. The authority sends
    /// one wake per switch and the loop drains it on the very next turn, so a
    /// handful outlasts any legitimate burst, and an unattested sender can
    /// queue no more than that, in its own lane, before the kernel refuses it.
    const WAKE_DEPTH: usize = 4;

    /// Calls one app may hold at the window endpoint: a request of each posted
    /// kind and a present for each of its windows, and one call per calling
    /// thread. Past it the app's own further posts wait for its replies. A
    /// fail-closed memory bound, paid by the app.
    const WINDOW_CALLER_DEPTH: usize = 64;

    /// Calls one producer may hold at the notification endpoint: notifications
    /// are infrequent and synchronous, one per calling thread (a fail-closed
    /// memory bound).
    const NOTIFY_CALLER_DEPTH: usize = 4;

    /// Calls the Switchboard's one attested publisher may hold at its endpoint:
    /// it posts change-driven and synchronously, so it stays tiny (a
    /// fail-closed memory bound).
    const SWITCHBOARD_DEPTH: usize = 4;

    /// Calls one caller may hold at the pinboard endpoint: an apply is a
    /// deliberate, user-driven act and synchronous (a fail-closed memory
    /// bound).
    const PINBOARD_CALLER_DEPTH: usize = 4;

    /// The sink this session records through — every cache's audit trail and
    /// the desktop's one-shot reveal witness. The shared cache constructors
    /// take a `'static` borrow, and the runtime sink is a unit value that
    /// owns nothing.
    static LOG_SINK: tairix_rt::LogSink = tairix_rt::LogSink;

    /// The name this program states its refusals under.
    const APP_NAME: &str = "desktop";

    /// The production pointer [`SeatEventReader`]: the seat-addressed
    /// `pointer_read` drain of the boot seat's pointer channel, owner-gated
    /// kernel-side against the live lease on every call.
    struct PointerReader;

    impl SeatEventReader for PointerReader {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
            let ret = tairix_rt::pointer_read(SEAT_PRIMARY, buf);
            if ret < 0 {
                return Err(Errno::from_syscall(ret));
            }
            // A count the address width cannot hold is refused, never
            // truncated into a shorter, decodable-looking record.
            usize::try_from(ret).map_err(|_| Errno::LengthOutOfRange)
        }
    }

    /// The production touch [`SeatEventReader`]: the seat-addressed
    /// `touch_read` drain of the boot seat's touch channel, owner-gated
    /// kernel-side against the live lease on every call.
    struct TouchReader;

    impl SeatEventReader for TouchReader {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
            let ret = tairix_rt::touch_read(SEAT_PRIMARY, buf);
            if ret < 0 {
                return Err(Errno::from_syscall(ret));
            }
            usize::try_from(ret).map_err(|_| Errno::LengthOutOfRange)
        }
    }

    /// The production keyboard [`SeatEventReader`]: the seat-addressed
    /// `keyboard_read` drain of the boot seat's keyboard channel,
    /// owner-gated kernel-side against the live lease on every call.
    struct KeyboardReader;

    impl SeatEventReader for KeyboardReader {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, Errno> {
            let ret = tairix_rt::keyboard_read(SEAT_PRIMARY, buf);
            if ret < 0 {
                return Err(Errno::from_syscall(ret));
            }
            // A count the address width cannot hold is refused, never
            // truncated into a shorter, decodable-looking record.
            usize::try_from(ret).map_err(|_| Errno::LengthOutOfRange)
        }
    }

    /// The production [`CallerIdentity`]: the kernel's `call_peer_origin`
    /// on the served window endpoint, so every request is attributed to
    /// the kernel-attested in-flight caller — never a claim the request
    /// carried. Each attested caller is retained so a reaped child pid
    /// resolves back to the client whose windows must be torn down, and so
    /// the icon bar can say which *application* a process is.
    struct RtWindowIdentity {
        peers: BTreeMap<u64, Peer>,
    }

    /// What the kernel attested about one window-channel client.
    ///
    /// The whole origin is decoded per request already, so keeping the app
    /// identity beside the process instance costs nothing and no extra
    /// syscall — and it is the only trustworthy answer to "which bundle is
    /// this?": it comes from the manifest the load gate verified, whoever
    /// started the process. A client not admitted from a signed bundle
    /// carries none, which is the absence the bar states as such.
    #[derive(Copy, Clone)]
    struct Peer {
        proc_id: ProcId,
        app: Option<tairix_abi::AppIdentity>,
    }

    impl RtWindowIdentity {
        const fn new() -> Self {
            Self {
                peers: BTreeMap::new(),
            }
        }

        /// Resolve (and forget) the client that ran as child `pid`.
        fn take_by_pid(&mut self, pid: u64) -> Option<ProcId> {
            self.peers.remove(&pid).map(|peer| peer.proc_id)
        }

        /// Resolve (and forget) the client whose event mailbox is
        /// `endpoint` — the owner a held-back send has just proved gone.
        ///
        /// Matched *forward*, by deriving each attested peer's mailbox and
        /// comparing, never by inverting the endpoint value back into a pid:
        /// the answer rests on the kernel-attested pid, exactly as the seat
        /// report's owner naming does.
        fn take_by_event_endpoint(&mut self, endpoint: u64) -> Option<ProcId> {
            let pid = self
                .peers
                .keys()
                .copied()
                .find(|pid| event_endpoint_for(*pid) == endpoint)?;
            self.take_by_pid(pid)
        }

        /// The kernel task id the attested client `id` called as, if it
        /// has called this session. The delegation target of a concluded
        /// pick: the pid came from `call_peer_origin`, never a wire claim.
        /// The instance it was attested under is held beside it, which is
        /// what a pid-only `fd_grant` cannot yet carry
        /// (`plans/OPEN-DEFECTS.md` D92).
        fn pid_of(&self, id: ProcId) -> Option<u64> {
            self.peers
                .iter()
                .find(|(_, peer)| peer.proc_id == id)
                .map(|(pid, _)| *pid)
        }

        /// The application the kernel attested `id` is running, if it was
        /// admitted from a signed bundle.
        ///
        /// The icon bar's whole attribution: it answers the same for a
        /// process the desktop launched, one a shell launched, and one another
        /// application launched, because the answer is the kernel's rather
        /// than the desktop's record of what it spawned.
        fn app_of(&self, id: ProcId) -> Option<tairix_abi::AppIdentity> {
            self.peers
                .values()
                .find(|peer| peer.proc_id == id)
                .and_then(|peer| peer.app)
        }

        /// The attested client that ran as child `pid`, *without* forgetting
        /// it — the inverse of [`pid_of`](Self::pid_of).
        ///
        /// How a relaunch reaches the instance it found in the launch table:
        /// the table names a pid, and an application-scoped event names a
        /// `ProcId`. Read-only, unlike [`take_by_pid`](Self::take_by_pid),
        /// which forgets because a reaped child is gone.
        fn proc_id_of(&self, pid: u64) -> Option<ProcId> {
            self.peers.get(&pid).map(|peer| peer.proc_id)
        }
    }

    impl CallerIdentity for RtWindowIdentity {
        fn caller(&mut self, ticket: u64) -> Result<ProcId, Errno> {
            let origin = tairix_rt::peer_origin(WINDOW_ENDPOINT, ticket)?;
            self.peers.insert(
                origin.pid(),
                Peer {
                    proc_id: origin.proc_id(),
                    app: origin.app().copied(),
                },
            );
            Ok(origin.proc_id())
        }

        fn caller_mailbox(&mut self, ticket: u64) -> Result<u64, Errno> {
            let origin = tairix_rt::peer_origin(WINDOW_ENDPOINT, ticket)?;
            Ok(tairix_abi::window_ipc::event_endpoint_for(origin.pid()))
        }

        fn caller_holds(&mut self, ticket: u64, cap: CapabilityId) -> Result<bool, Errno> {
            // The kernel's own attestation of the in-flight caller, not
            // anything the caller said: the summary is minted by the kernel
            // at call time and cannot be forged from user space.
            Ok(tairix_rt::peer_origin(WINDOW_ENDPOINT, ticket)?
                .capabilities()
                .holds_cap(cap))
        }

        fn caller_app(&mut self, ticket: u64) -> Result<Option<tairix_abi::AppIdentity>, Errno> {
            Ok(tairix_rt::peer_origin(WINDOW_ENDPOINT, ticket)?
                .app()
                .copied())
        }
    }

    /// The production [`EventSink`]: one non-blocking `ipc_send` to the
    /// owning app's event port per event. To avoid flooding an app with
    /// samples it can only act on the newest of, the shell coalesces
    /// adjacent motions naming the same window; every remaining outcome is
    /// a non-blocking send. The send never parks this session (a full
    /// mailbox or a dead port is a typed refusal), so a wedged app can
    /// never wedge the desktop.
    ///
    /// A refused send is **held**, not dropped ([`HoldBack`]): the mailbox
    /// is a bounded resource and a merely slow app fills it, so dropping
    /// would cost the app a resize it cannot re-derive or a picker
    /// conclusion it is owed exactly once. The sink arms a room member on
    /// the destination's port, and the loop's [`Self::flush`] sends what is
    /// owed the moment the app drains or the session's own spent budget frees,
    /// the kernel waking the member for either — it never polls for capacity
    /// and never blocks on the app. What the kernel had no memory for, which
    /// nothing wakes for, goes with a flush the loop times one pause later.
    ///
    /// Every send outcome doubles as responsiveness evidence: the wrapped
    /// [`HangTracker`] folds each `WouldBlock` back-pressure refusal and
    /// each accepted delivery into per-owner "not responding" verdicts
    /// (keyed by the event-mailbox endpoint, which embeds the owning task
    /// id), and the loop drains [`take_changed`](Self::take_changed) once
    /// per wake to bring the taskbar capsule in step. Time is stamped only
    /// on the delivery paths, so an idle desktop reads no clock.
    struct RtEventSink {
        vigil: HangTracker,
        changed: bool,
        /// The wait-set the room members are armed on.
        set: u64,
        held: HoldBack,
        /// The pause after which what the kernel had no memory for is offered
        /// again.
        pause: MemoryPause,
    }

    impl RtEventSink {
        /// A sink with no delivery evidence yet, arming its room members on
        /// `set`.
        const fn new(set: u64) -> Self {
            Self {
                vigil: HangTracker::new(),
                changed: false,
                set,
                held: HoldBack::new(),
                pause: MemoryPause::new(),
            }
        }

        /// Watch `endpoint` for room, so the app draining its mailbox wakes
        /// the loop to send what it is owed.
        fn arm(&self, endpoint: u64) -> Result<(), Errno> {
            let ret = tairix_rt::waitset_ctl(
                self.set,
                WaitSetOp::Add,
                WaitSourceKind::PortRoom,
                endpoint,
                HOLDBACK_TOKEN,
            );
            if ret == 0 {
                Ok(())
            } else {
                Err(Errno::from_syscall(ret))
            }
        }

        /// Stop watching `endpoint` for room — it is owed nothing, or its
        /// owner is gone.
        fn disarm(&self, endpoint: u64) {
            let _ = tairix_rt::waitset_ctl(
                self.set,
                WaitSetOp::Del,
                WaitSourceKind::PortRoom,
                endpoint,
                HOLDBACK_TOKEN,
            );
        }

        /// Watch `endpoint` for room, answering whether it is watched. What an
        /// unwatchable destination is owed could never go out, and may be
        /// events it cannot do without, so its debt is dropped — a destination
        /// is watched exactly while owed — and its app is to be taken as gone.
        fn watch_room(&mut self, endpoint: u64) -> bool {
            if self.arm(endpoint).is_ok() {
                return true;
            }
            let _ = self.held.forget(endpoint);
            self.disarm(endpoint);
            io::write_stderr_line("desktop: cannot watch an app's mailbox for room");
            false
        }

        /// How long the loop may park before the owed flush is due, if one is.
        fn retry_in(&self, now_ns: u64) -> Option<u64> {
            self.pause.ends().map(|at| at.saturating_sub(now_ns))
        }

        /// Whether the owed flush is due, reading the clock only while one is
        /// owed; the flush it is due for times the next pause.
        fn retry_due(&mut self) -> bool {
            self.pause.ran_out(tairix_rt::clock_get)
        }

        /// Keep the timed flush running while an event held waits for kernel
        /// memory, and stop it once none does.
        fn keep_pause(&mut self) {
            self.pause.keep(self.held.starved(), tairix_rt::clock_get);
        }

        /// One non-blocking app-ward send, folding its outcome into the
        /// responsiveness evidence.
        ///
        /// Free of `self` so the hold-back can be borrowed across it: both
        /// the first attempt and every later flush go through this one
        /// definition, so an event's send and its evidence can never differ
        /// by which path carried it.
        fn post(
            vigil: &mut HangTracker,
            changed: &mut bool,
            endpoint: u64,
            event: &WindowEvent,
        ) -> Result<(), Errno> {
            let Err(error) = tairix_rt::ipc_send(endpoint, &event.to_le_bytes()) else {
                *changed |= vigil.note_delivered(endpoint);
                return Ok(());
            };
            *changed |= vigil.note_refused(endpoint, error, tairix_rt::clock_get());
            Err(error)
        }

        /// Send what each destination is owed, as far as its mailbox now
        /// allows, and report the owners the sends proved gone, or whose room
        /// cannot be watched, so the loop can tear their windows down.
        fn flush(&mut self) -> Vec<u64> {
            let vigil = &mut self.vigil;
            let changed = &mut self.changed;
            let mut report = self
                .held
                .flush(|endpoint, event| Self::post(vigil, changed, endpoint, event));
            for endpoint in report.settled.iter().chain(&report.gone) {
                self.disarm(*endpoint);
            }
            for endpoint in core::mem::take(&mut report.watch) {
                if !self.watch_room(endpoint) {
                    report.gone.push(endpoint);
                }
            }
            if report.dropped > 0 {
                io::write_stderr_line("desktop: events an app could never take were dropped");
            }
            self.keep_pause();
            report.gone
        }

        /// Whether the unresponsive set changed since the last drain,
        /// clearing the latch.
        fn take_changed(&mut self) -> bool {
            core::mem::take(&mut self.changed)
        }

        /// How many window owners are currently flagged unresponsive.
        fn unresponsive_count(&self) -> u16 {
            self.vigil.unresponsive_count()
        }

        /// The flagged owners' event-mailbox endpoints, walked without
        /// allocating so the seat report can name a bounded few of them.
        fn unresponsive_endpoints(&self) -> impl Iterator<Item = u64> + '_ {
            self.vigil.unresponsive_owners()
        }

        /// Drop every verdict held against a reaped child's event mailbox,
        /// and everything still owed to it — a dead app is not a hung app,
        /// a recycled task id must start clean, and events owed to a corpse
        /// have nowhere to land.
        fn forget_owner(&mut self, pid: u64) {
            let endpoint = event_endpoint_for(pid);
            self.changed |= self.vigil.forget(endpoint);
            if self.held.forget(endpoint) {
                self.disarm(endpoint);
            }
        }
    }

    impl EventSink for RtEventSink {
        fn deliver(&mut self, endpoint: u64, event: &WindowEvent) -> Result<(), Errno> {
            let vigil = &mut self.vigil;
            let changed = &mut self.changed;
            // Back-pressure means the app is behind, not gone: the event is
            // held rather than dropped, and the destination watched for room.
            let outcome = self.held.deliver(endpoint, event, |to, event| {
                Self::post(vigil, changed, to, event)
            })?;
            let (watch, behind) = match outcome {
                Delivery::Sent => return Ok(()),
                // Nothing wakes for kernel memory, and the app is not at fault.
                Delivery::Starved => {
                    self.pause.keep(true, tairix_rt::clock_get);
                    return Ok(());
                }
                Delivery::Owed { watch } => (watch, true),
                Delivery::OverBudget { watch } => (watch, false),
            };
            // An event the app is behind on could not be delivered, so the
            // responsiveness evidence stands whether the refusal came from
            // this send or from the debt this one joined: only a delivery the
            // owner accepts clears it. The session's own budget is not the
            // app's to answer for.
            if behind {
                self.changed |=
                    self.vigil
                        .note_refused(endpoint, Errno::WouldBlock, tairix_rt::clock_get());
            }
            if watch && !self.watch_room(endpoint) {
                return Err(Errno::NotFound);
            }
            Ok(())
        }

        fn holds_render(&self, endpoint: u64, window_id: u64) -> bool {
            self.held.holds_render(endpoint, window_id)
        }
    }

    /// The thread the screen lock's passwords go to the per-console elevation
    /// broker on — the login supervisor that started this session — so the key
    /// it derives from each never stalls the compositor.
    type UnlockWorker = Worker<(), UnlockAsk, UnlockAnswer>;

    /// What the screen lock asks through: its worker, which carries a question
    /// on the serve loop's own task only where the kernel granted no thread.
    struct ToBroker(alloc::sync::Arc<UnlockWorker>);

    impl Courier for ToBroker {
        type Answer = UnlockAnswer;

        /// The account name is not carried: the broker re-reads the caller's
        /// identity from the kernel and checks the password against that uid,
        /// so naming an account could only ever ask for one this process is
        /// not.
        fn send(
            &mut self,
            ticket: Ticket,
            _account: &str,
            password: &str,
        ) -> Result<(), Self::Answer> {
            let ask = UnlockAsk::new(ticket, password)
                .ok_or(UnlockAnswer::of(ticket, Verdict::Unreachable))?;
            self.0.submit(ask);
            Ok(())
        }

        fn receive(&mut self) -> Option<Self::Answer> {
            self.0.collect()
        }
    }

    /// The broker's answer to one question at the lock, its password erased
    /// before returning. A password that is not text was never one the broker
    /// could check.
    fn ask_broker(_: &mut (), ask: &mut UnlockAsk) -> UnlockAnswer {
        let answer = ask.password().map_or(
            UnlockAnswer::of(ask.ticket(), Verdict::Refused),
            |password| broker_answer(ask.ticket(), password),
        );
        ask.erase();
        answer
    }

    /// The broker's answer on `password` for this process's own attested
    /// identity, about the offer `ticket` names. The runtime client erases its
    /// request buffer on every return path.
    fn broker_answer(ticket: Ticket, password: &str) -> UnlockAnswer {
        let mut reply = [0u8; ELEVATE_MAX_REPLY];
        let request = ElevateRequest::Verify {
            password: Secret::new(password),
        };
        match tairix_rt::elevate(&request, &mut reply) {
            Ok(ElevateReply::Verified) => UnlockAnswer::of(ticket, Verdict::Verified),
            Ok(ElevateReply::Refused(_)) => UnlockAnswer::of(ticket, Verdict::Refused),
            // Nothing was checked: the password is refused, and the lock says
            // how long until one will be.
            Ok(ElevateReply::Cooling { retry_after }) => UnlockAnswer {
                ticket,
                verdict: Verdict::Refused,
                retry_after,
            },
            // Every other reply answers a request this surface did not send.
            // A broker that sent one is not speaking this protocol, and a lock
            // does not open on a reply it did not understand.
            Ok(
                ElevateReply::Completed { .. }
                | ElevateReply::Launched { .. }
                | ElevateReply::Wired(_)
                | ElevateReply::Captured { .. }
                | ElevateReply::Overran { .. }
                | ElevateReply::Authorisers { .. },
            )
            | Err(_) => UnlockAnswer::of(ticket, Verdict::Unreachable),
        }
    }

    /// Secure the screen, however it was asked for: the icon bar's Lock row,
    /// the idle policy, or the desktop's Settings application.
    ///
    /// The prompts go down first: an unanswered question must not sit behind a
    /// lock where the user cannot see what they are agreeing to. So does a
    /// drag, dropped on nothing, rather than carried on past the unlock. A lock
    /// that could not be put up says so rather than leaving the user believing
    /// the screen is secured.
    fn lock_screen<S: DirectorySource>(
        lock: &mut ScreenLock,
        (confirm, elevate, resolution): (
            &mut ConfirmPrompt,
            &mut AuthorisePrompt,
            &mut ResolutionPrompt,
        ),
        named: (&str, &str),
        (shell, compositor, desktop): (&mut DesktopShell, &mut Compositor, &mut Desktop<S>),
        server: &mut WindowServer<RtShmMapper>,
        sink: &mut RtEventSink,
    ) {
        confirm.abandon(shell, compositor);
        report_elevation(elevate.abandon(compositor));
        resolution.abandon(shell, compositor);
        match shell.end_drag(compositor, false) {
            // A source whose port has gone is torn down by its next delivery.
            Some(DragEnd::Items(ended)) => {
                if let Some(offer) = ended.offer {
                    let _ = server.deliver_event(sink, &offer.event());
                }
                let _ = server.conclude_drag(sink, ended.source, ended.ended);
            }
            Some(DragEnd::Save(SaveEnd {
                offer: Some(offer), ..
            })) => {
                let _ = server.deliver_event(sink, &offer.event());
            }
            Some(DragEnd::Desktop(DesktopEnd { offer, .. })) => {
                if let Some(offer) = offer {
                    let _ = server.deliver_event(sink, &offer.event());
                }
                let layout = shell.desktop_layout(compositor, desktop);
                let mut damage = Region::new();
                desktop.restore_lent(&layout, &mut damage);
                shell.present_desktop_area(compositor, desktop, &damage);
            }
            Some(DragEnd::Save(_)) | None => {}
        }
        if !lock.engage(named, shell, compositor) {
            io::write_stderr_line("desktop: could not lock the screen; it is still open");
        }
    }

    /// What starting a saver bundle draws on that only the session's loop
    /// holds: the savers the desktop offers, who is signed in and where, the
    /// machine's cores and memory, and the shipped pictures, of which the
    /// positions the slides feed offers are recorded in `slides`.
    struct SaverHosting<'a> {
        savers: &'a SaverCatalog,
        reaper: &'a alloc::sync::Arc<ChildReaper>,
        identity: (&'a str, &'a str),
        machine: (u16, u64),
        catalog: &'a [WallpaperName],
        slides: &'a mut alloc::vec::Vec<usize>,
    }

    /// Cover the screen with the screensaver `asked` names over the desktop's
    /// `backdrop`, as a preview when `preview`: the one start the idle
    /// deadline and a Test request both go through.
    fn start_screensaver(
        saver: &mut DesktopSaver,
        asked: &ScreensaverPreview,
        backdrop: tairix_wallpaper::Backdrop,
        (shell, compositor): (&DesktopShell, &mut Compositor),
        hosting: &mut SaverHosting<'_>,
        (now_ns, preview): (u64, bool),
    ) {
        if !saver.would_start() {
            return;
        }
        let screen = compositor.screen_rect();
        // Only the dimmed screensaver builds the backdrop's ground, so no
        // other pays for a full-screen surface it discards.
        let ground = match asked.choice {
            ScreensaverChoice::Dim => shell.backdrop_ground(backdrop, screen.width, screen.height),
            ScreensaverChoice::Blank | ScreensaverChoice::Saver(_) => None,
        };
        let mut seed = [0u8; tairix_abi::saver_ipc::SAVER_SEED_LEN];
        let bundle = match &asked.choice {
            ScreensaverChoice::Saver(id) => match hosting.savers.offered(&asked.choice) {
                None => {
                    app::report(
                        APP_NAME,
                        format_args!(
                            "the screensaver {} is not one the desktop offers; the screen is \
                             kept black",
                            id.as_str()
                        ),
                    );
                    None
                }
                Some(bundle) => match tairix_rt::random_fill(&mut seed) {
                    Ok(()) => Some(bundle),
                    Err(errno) => {
                        app::report(
                            APP_NAME,
                            format_args!(
                                "no randomness for the screensaver {} ({errno}); the screen is \
                                 kept black",
                                bundle.title
                            ),
                        );
                        None
                    }
                },
            },
            ScreensaverChoice::Blank | ScreensaverChoice::Dim => None,
        };
        hosting.slides.clear();
        if bundle.is_some_and(|bundle| bundle.feeds.slides())
            && slide_positions(hosting.catalog, &asked.slides, hosting.slides).is_err()
        {
            app::report(
                APP_NAME,
                format_args!("no memory to offer the screensaver its slides"),
            );
        }
        let theme = compositor.theme();
        let fonts = theme.fonts();
        let look = (
            theme.appearance(),
            tairix_abi::desktop::DesktopText::new(fonts.ui_family(), fonts.base_size_px()).ok(),
            theme.contrast(),
            theme.density(),
            theme.motion().reduced_motion(),
        );
        let mut launcher = RtSaverLauncher {
            reaper: hosting.reaper,
        };
        let setup = SaverSetup {
            ground,
            launch: bundle.map(|bundle| SaverLaunch {
                saver: bundle,
                context: SaverContext {
                    screen: (screen.width, screen.height),
                    scale: compositor.scale(),
                    refresh: compositor.refresh_rate(),
                    look,
                    seed,
                    pressure: tairix_rt::pressure::gauge().band(),
                    machine: hosting.machine,
                    identity: hosting.identity,
                    slides: u16::try_from(hosting.slides.len()).unwrap_or(u16::MAX),
                    values: &asked.values,
                    names: asked.names,
                },
                launcher: &mut launcher,
            }),
        };
        let started = if preview {
            saver.start_preview(setup, compositor, now_ns)
        } else {
            saver.start(setup, compositor, now_ns)
        };
        if let Err(end) = started {
            report_saver_end(&end);
        }
    }

    /// Say why the saver drawing the screensaver was ended.
    fn report_saver_end(end: &SaverEnd) {
        app::report(APP_NAME, format_args!("{end}"));
    }

    /// Drive the seat's pointer by `policy`: the button order and speed, what
    /// a touch means, and the density a touchscreen is measured by.
    fn drive_pointer<C, T>(pointer: &mut DeviceInputSource<C, T>, policy: &InputPolicy) {
        pointer.set_policy(policy.primary, policy.speed);
        pointer.set_touch(policy.touch);
        pointer.set_density(policy.density);
    }

    /// Classify one drain fault: losing the seat is the session's normal
    /// fail-loud teardown; anything else is an untrustworthy input stream.
    /// Either way the session is ending, so the shell's disposable-UI caches
    /// are wiped before the exit code is returned.
    fn drain_fault(shell: &mut DesktopShell, compositor: &mut Compositor, err: Errno) -> i32 {
        shell.teardown(compositor);
        match err {
            Errno::SeatRevoked | Errno::SeatNotOwner => app::fail(
                APP_NAME,
                EXIT_SEAT_LOST,
                "seat lease lost; tearing the session down",
            ),
            _ => app::fail(APP_NAME, EXIT_INPUT_FAULT, "seat input drain faulted"),
        }
    }

    /// Step everything the session animates to `now_ns` when `pacer` has a
    /// frame due, so the frame presented next carries it: the desktop's
    /// screen fade, the windows' shadows rising and resting as the focus
    /// moves, the program library rising, arriving and leaving, the pointer
    /// aids, what a drag carries gathering under the pointer, the locked
    /// screen's own surface, a credential
    /// prompt's password marker, the bar's clock, and the backdrop dissolving
    /// into another. Between frames only the pointer aids take in
    /// where the pointer is. All of them are idle once nothing is in flight,
    /// which is what leaves an idle desktop's park indefinite; what each owes
    /// next is folded through [`FramePacer::step_deadline_ns`].
    fn animate<S: DirectorySource>(
        fade: &mut ScreenFade,
        (lock, elevate): (&mut ScreenLock, &mut AuthorisePrompt),
        shell: &mut DesktopShell,
        desktop: &Desktop<S>,
        (compositor, pacer): (&mut Compositor, &FramePacer),
        now_ns: u64,
    ) {
        shell.observe_pointer_aids(now_ns, compositor);
        if !pacer.due(now_ns, compositor.refresh_rate()) {
            return;
        }
        fade.advance(now_ns, compositor);
        compositor.step_elevations(now_ns);
        shell.advance_library(compositor, now_ns);
        shell.advance_pointer_aids(now_ns, compositor);
        shell.advance_drag(now_ns, compositor);
        lock.advance(now_ns, shell, compositor);
        elevate.resolve_picture(shell, compositor.scale());
        if elevate.advance(now_ns, shell, compositor) {
            // The question is drawn and asking: announce it so a host that
            // must type into it waits on a real surface.
            log_info(ELEVATE_PROMPT_SHOWN, ELEVATE_PROMPT_SHOWN_MESSAGE, &[]);
        }
        tick_clock(shell, compositor, now_ns);
        // A backdrop dissolving into another is a repaint of the desktop
        // layer rather than a compositor state change, so each frame of it is
        // the layer drawn again. Only a frame that changed the ground costs
        // one, and only a running fade can have changed it — which is why the
        // reveal witness may be answered from in here: the wallpaper is on
        // screen but still arriving, and a user cannot yet see the desktop
        // they configured.
        if shell.advance_backdrop(now_ns) {
            shell.present_desktop(compositor, desktop);
            fade.set_awaiting_backdrop(!shell.backdrop_settled());
        }
    }

    /// Carry the hosted saver on to `now_ns`, on every turn rather than only a
    /// frame the pacer has due: its frames are asked for at the times it asks,
    /// whatever the session's own animations are doing, and its deadlines are
    /// held on every wake. The clock is read first, so a saver drawing the
    /// minute that has just turned is told it.
    ///
    /// What that and the frame just shown laid arrive after this wake's frame
    /// was decided, so `pacer` holds them for the next: on a display `ready`
    /// for one, the period's end wakes the park for it.
    fn advance_saver(
        (saver, lock): (&mut DesktopSaver, &ScreenLock),
        (shell, compositor, pacer): (&mut DesktopShell, &mut Compositor, &mut FramePacer),
        (ready, pending): (bool, bool),
        now_ns: u64,
    ) {
        tick_clock(shell, compositor, now_ns);
        if let Some(end) = saver.advance(now_ns, &shell.clock().saver_reading(), compositor) {
            report_saver_end(&end);
        }
        if presentable(saver, lock, compositor) {
            pacer.hold(compositor.has_damage() || pending, ready);
        }
    }

    /// Show every window's newest presented frame, as the frame about to be
    /// composed: an application presenting faster than the desktop draws
    /// costs the session one conversion a frame.
    fn latch_frames(
        server: &mut WindowServer<RtShmMapper>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        windows: &mut SessionWindows,
        seat_held: bool,
    ) {
        server.latch(&mut FrameShow {
            shell,
            compositor,
            windows,
            seat_held,
        });
    }

    /// Whether a frame may go to the display: none while the saver holds it
    /// dark or draws its frames straight into it, nor while the lock does not
    /// cover the session.
    fn presentable(saver: &DesktopSaver, lock: &ScreenLock, compositor: &Compositor) -> bool {
        !saver.is_dark() && !saver.scans_out() && !lock.uncovered(compositor)
    }

    /// Read the wall clock once the lines it last produced have gone stale,
    /// putting new ones on the bar; answers whether the bar was presented.
    ///
    /// Run every frame, but the clock owns the cadence: it is due at the next
    /// local minute, the deadline the park is shortened to, so an idle desktop
    /// reads it once a minute however often it wakes. A wall clock stepped
    /// meanwhile reaches the bar at that minute, there being no notice of a
    /// step to wait on. A refused read leaves the bar as it was and asks again
    /// a minute later.
    fn tick_clock(shell: &mut DesktopShell, compositor: &mut Compositor, now_ns: u64) -> bool {
        shell.clock().is_due(now_ns)
            && shell.tell_time(compositor, tairix_rt::wall_time().ok(), now_ns)
    }

    /// The planted zone store and the descriptor it was read through, or
    /// `None` — stated once — where it cannot be had and the clock reads UTC.
    fn load_zones() -> Option<HeldZones> {
        let file = match tairix_rt::File::open(
            tairix_tz::STORE_PATH.as_bytes(),
            tairix_abi::fs::OpenFlags::READ,
        ) {
            Ok(file) => file,
            Err(raw) => {
                let error = tairix_tz::LoadError::Unreadable(Errno::from_syscall(raw));
                app::report(APP_NAME, format_args!("{error}; the clock reads UTC"));
                return None;
            }
        };
        match tairix_tz::load(&file) {
            Ok(info) => {
                let fd = file.fd();
                // Kept open for the life of the session, as the store itself
                // is: Settings is delegated the store through it.
                core::mem::forget(file);
                Some(HeldZones { info, fd })
            }
            Err(error) => {
                app::report(APP_NAME, format_args!("{error}; the clock reads UTC"));
                None
            }
        }
    }

    /// State a choice of `settings` the zone store does not hold, which the
    /// clock passed over for the store's default.
    ///
    /// Said at bring-up alone: an apply that changes either choice to one the
    /// store lacks is refused, so no other adoption can bring one.
    fn report_passed_over(adopted: CivilAdoption, settings: &DesktopSettings) {
        if let Some(name) = settings
            .time_zone
            .as_ref()
            .filter(|_| adopted.zone != Decided::Preference(0) && adopted.zone != Decided::NoStore)
        {
            app::report(
                APP_NAME,
                format_args!(
                    "the zone {} is not in the zone store; the clock reads the machine's",
                    name.as_str()
                ),
            );
        }
        if let (Decided::StoreDefault, Some(code)) = (adopted.region, settings.region.as_ref()) {
            app::report(
                APP_NAME,
                format_args!(
                    "the region {} is not in the zone store; the clock is written as its default's",
                    code.as_str()
                ),
            );
        }
    }

    /// Dissolve the session's screen to black, presenting every frame, and
    /// return once it is dark.
    ///
    /// The last thing a session draws before it hands the seat on cleared,
    /// so the desktop dims into the black the login screen appears out of
    /// rather than vanishing mid-frame. Bounded by the fade's own span, and
    /// a no-op under a reduced-motion theme, which is dark from its first
    /// frame.
    ///
    /// Paced on the runtime's timed park, not the session wait-set: the
    /// sources this loop is not serving would report ready on every re-park
    /// and spin a core through the whole fade. A refused present stops the
    /// dim where it got to — nothing here can act on it, and the seat is
    /// handed on cleared regardless, so the screen still ends black.
    fn fade_to_black(
        fade: &mut ScreenFade,
        compositor: &mut Compositor,
        display: &mut Option<Ring>,
    ) {
        let Some(display) = display.as_mut() else {
            return;
        };
        fade.depart(tairix_rt::clock_get(), compositor);
        loop {
            display.take_events();
            if compositor.present(display).is_err() {
                return;
            }
            if fade.settled() {
                return;
            }
            tairix_rt::park_ns(fade.park_deadline_ns(
                tairix_rt::clock_get(),
                WAITSET_TIMEOUT_NONE,
                compositor.refresh_rate(),
            ));
            fade.advance(tairix_rt::clock_get(), compositor);
        }
    }

    /// Present the composited damage through the remote display, mapping a
    /// refusal onto the session's exit codes. The service refuses a caller
    /// whose lease is no longer live (`SeatRevoked` from the kernel's
    /// per-request check; a stale owner surfaces as a permission refusal),
    /// so a lost seat is observed here exactly as on a drain, and ends the
    /// session with the shell's disposable-UI caches wiped first.
    ///
    /// Frames that no longer describe the output — it moved, or a restarted
    /// service holds no configuration for them — are not a failure. They are
    /// `suspended` instead, and presented nothing more, until the service's
    /// next announcement of the output, which both send: the session converges
    /// on that, and a service that refused frames without announcing anything
    /// can make the session wait but never spin.
    ///
    /// A background or dormant session owns no frame ring, presents nothing,
    /// and answers `Ok(false)`: having no screen to draw on is not a failure.
    ///
    /// A frame that did reach the display is where the desktop's one-shot
    /// reveal witness is announced, so the record can only follow pixels
    /// this session actually put on the screen, and with it every other
    /// surface this frame was the first to carry
    /// ([`report_surfaces_shown`]). Answers whether one did.
    #[allow(clippy::too_many_arguments)] // Every surface a present may be the first showing of.
    fn present<
        S: tairix_browse::DirectorySource,
        F: PickSources<S>,
        D: tairix_browse::DirectorySource,
    >(
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        display: &mut Option<Ring>,
        fade: &mut ScreenFade,
        windows: &mut SessionWindows,
        menu: &mut MenuChain,
        picker: &mut SessionPicker<S, F>,
        (apps, desktop): (&mut AppBarService, &mut Desktop<D>),
        suspended: &mut Option<bool>,
        text: TextDrawn<'_>,
    ) -> Result<bool, i32> {
        let Some(display) = display.as_mut() else {
            return Ok(false);
        };
        if suspended.is_some() {
            return Ok(false);
        }
        let refused = match compositor.present(display) {
            Ok(()) => {
                fade.presented(&LOG_SINK);
                let bars = (apps, desktop);
                report_surfaces_shown(shell, compositor, fade, windows, menu, picker, bars, text);
                return Ok(true);
            }
            Err(refused) => refused,
        };
        match Refusal::of(refused) {
            Refusal::Follow { forgotten } => {
                *suspended = Some(forgotten);
                Ok(false)
            }
            Refusal::SeatLost => {
                shell.teardown(compositor);
                Err(app::fail(
                    APP_NAME,
                    EXIT_SEAT_LOST,
                    "seat lease lost; tearing the session down",
                ))
            }
            Refusal::Owed => Ok(false),
            Refusal::Failed => {
                shell.teardown(compositor);
                Err(app::fail(
                    APP_NAME,
                    EXIT_PRESENT_FAILED,
                    "display present refused",
                ))
            }
        }
    }

    /// Record `id` on the session's sink at `Info`: a witness that a surface
    /// reached the screen, or a routine decision worth attributing.
    fn log_info(id: EventId, message: &str, fields: &[LogField<'_>]) {
        log(
            &LOG_SINK,
            &LogEvent {
                level: LogLevel::Info,
                id,
                message,
                fields,
            },
        );
    }

    /// Announce every surface this frame was the first to carry: each served
    /// window's first painted frame and each retitle of one already shown, a
    /// newly drawn icon-bar slot and the settled strip, the settled desktop
    /// icons, a change of the desktop's look, the menu chain, the trusted
    /// picker, and the program-library popup.
    ///
    /// Called only after a present reached the display, because until the frame
    /// lands nobody has seen any of them. Each witness is one-shot in its own
    /// owner, so an ordinary frame costs one walk of the served windows and a
    /// bool test for everything else.
    #[allow(clippy::too_many_arguments)] // Every surface a frame may be the first showing of.
    fn report_surfaces_shown<
        S: tairix_browse::DirectorySource,
        F: PickSources<S>,
        D: tairix_browse::DirectorySource,
    >(
        shell: &mut DesktopShell,
        compositor: &Compositor,
        fade: &ScreenFade,
        windows: &mut SessionWindows,
        menu: &mut MenuChain,
        picker: &mut SessionPicker<S, F>,
        (apps, desktop): (&mut AppBarService, &mut Desktop<D>),
        text: TextDrawn<'_>,
    ) {
        let window = |window| {
            [LogField {
                key: "window",
                value: LogFieldValue::UnsignedInt(window),
            }]
        };
        // The compositor's own record of how this frame reached the display,
        // so a witness names the path the frame took rather than one assumed.
        let presentation = compositor.presentation();
        // Asked after the frame drew, so the glyphs its titles missed count.
        let settled = || {
            text.settled()
                && compositor
                    .next_elevation_frame_in(tairix_rt::clock_get())
                    .is_none()
        };
        windows.report_on_screen(
            settled,
            |wm| compositor.on_display(wm),
            |id| log_info(WINDOW_SHOWN, WINDOW_SHOWN_MESSAGE, &window(id)),
            |id| log_info(WINDOW_RETITLED, WINDOW_RETITLED_MESSAGE, &window(id)),
            |window, state, extent| {
                let Some(path) = presentation.map(Presentation::as_str) else {
                    return;
                };
                let record = SizedRecord {
                    window,
                    state: size_state_name(state),
                    extent,
                    path,
                };
                log_info(WINDOW_SIZED, WINDOW_SIZED_MESSAGE, &record.fields());
            },
        );
        shell.report_restyled(fade.revealed(), |appearance| {
            let field = LogField {
                key: "appearance",
                value: LogFieldValue::Str(appearance.as_str()),
            };
            log_info(DESKTOP_RESTYLED, DESKTOP_RESTYLED_MESSAGE, &[field]);
        });
        shell.report_resized(fade.revealed(), compositor.screen_rect(), |screen| {
            let record = ResizedRecord {
                width: screen.width,
                height: screen.height,
            };
            log_info(DESKTOP_RESIZED, DESKTOP_RESIZED_MESSAGE, &record.fields());
        });
        apps.report_newly_shown(|owner| {
            let mut hex = [0u8; tairix_abi::PROC_ID_HEX_LEN];
            let field = LogField {
                key: "app",
                value: LogFieldValue::Str(owner.write_hex(&mut hex)),
            };
            log_info(APP_BAR_SLOT_SHOWN, APP_BAR_SLOT_SHOWN_MESSAGE, &[field]);
        });
        // After the reveal witness above, because that is the half of
        // this fact the fade owns and this frame may be the one that
        // gave it.
        apps.report_settled(fade.revealed(), || {
            log_info(APP_BAR_SETTLED, APP_BAR_SETTLED_MESSAGE, &[]);
        });
        desktop.report_settled(
            fade.revealed(),
            |desktop| shell.desktop_drawn_as_kept(compositor, desktop),
            || log_info(PINBOARD_SETTLED, PINBOARD_SETTLED_MESSAGE, &[]),
        );
        menu.report_newly_shown(|owner| {
            let owner = match owner {
                ChainOwner::Window { .. } => "window",
                ChainOwner::Backdrop => "backdrop",
                ChainOwner::Picker { .. } => "picker",
                ChainOwner::Bar(_) => "bar",
                ChainOwner::Capture => "capture",
            };
            let field = LogField {
                key: "owner",
                value: LogFieldValue::Str(owner),
            };
            log_info(MENU_SHOWN, MENU_SHOWN_MESSAGE, &[field]);
        });
        picker.report_newly_shown(tairix_rt::clock_get(), true, || {
            log_info(PICKER_SHOWN, PICKER_SHOWN_MESSAGE, &[]);
        });
        shell
            .session_mut()
            .taskbar_mut()
            .report_library_shown(|| log_info(LIBRARY_SHOWN, LIBRARY_SHOWN_MESSAGE, &[]));
    }

    /// Attest the producer of a pending notification call, decode the request
    /// fail-closed, and serve it held to the user's notification `policy`,
    /// returning the status the producer receives.
    ///
    /// The producer's identity is the kernel-attested `call_peer_origin` on
    /// the notification endpoint, never a wire claim, so a notification is
    /// always keyed to the service that actually posted it. An unattestable
    /// caller or a malformed request is a typed refusal (fail closed) and
    /// never mutates the model.
    fn serve_notify(
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        policy: &tairix_wallpaper::NotifyPolicy,
        ticket: u64,
        request: &[u8],
    ) -> Result<Option<SoundEvent>, Errno> {
        let origin = tairix_rt::peer_origin(NOTIFY_ENDPOINT, ticket)?;
        let request = NotifyRequest::from_bytes(request)?;
        shell.serve_notify(compositor, &origin, request, policy)
    }

    /// Attest the caller of a pending Switchboard call from the kernel and
    /// serve it through the shared, host-tested policy, returning what the
    /// caller is answered with.
    ///
    /// Only a Switchboard child this session spawned may call: the
    /// caller's kernel-attested `call_peer_origin` pid must hold a launch
    /// record of this session's own naming the service's bundle path.
    /// Anything else — a foreign process, an orphan of an earlier session,
    /// a copy launched by hand — is a typed refusal, stated on `stderr`
    /// and on the audit trail, and never mutates the model (fail closed).
    /// A malformed frame, and an owner-directed operation naming an owner
    /// this session cannot act on, refuse the same way.
    fn serve_switchboard(
        serve: SwitchboardServe<'_>,
        ticket: u64,
        request: &[u8],
    ) -> Result<SwitchboardOutcome, Errno> {
        let origin = tairix_rt::peer_origin(SWITCHBOARD_ENDPOINT, ticket)?;
        serve_switchboard_request(serve, origin.pid(), request).map_err(|refusal| {
            let msg = refusal.reason();
            app::report(APP_NAME, format_args!("{msg}"));
            log(
                &LOG_SINK,
                &LogEvent {
                    level: LogLevel::Warn,
                    id: SWITCHBOARD_CALL_REFUSED,
                    message: msg,
                    fields: &[LogField {
                        key: "caller",
                        value: LogFieldValue::UnsignedInt(origin.pid()),
                    }],
                },
            );
            refusal.errno()
        })
    }

    /// The live window ownership an `ActivateOwner` is validated against:
    /// the window engine's own attested owner records, resolved through the
    /// one `window_of_app` every other owner lookup in this session uses.
    ///
    /// The owner arrives as a task id — what the tray feed names a task by —
    /// so it is resolved to the attested instance the records are keyed on
    /// before any window is matched.
    struct SessionOwnerWindows<'a> {
        server: &'a WindowServer<RtShmMapper>,
        windows: &'a SessionWindows,
        identity: &'a RtWindowIdentity,
    }

    impl OwnerWindow for SessionOwnerWindows<'_> {
        fn window_of(&self, owner: u64) -> Option<tairix_wm::WindowId> {
            window_of_app(self.identity.proc_id_of(owner)?, self.server, self.windows)
        }
    }

    /// The production [`SwitchboardMailbox`]: one non-blocking `ipc_send`
    /// to the live monitor's own per-instance command mailbox.
    ///
    /// The send never parks the desktop and never retries in a loop: it
    /// makes one attempt and answers whether the mailbox took it, leaving
    /// the caller to decide whether the command is worth holding for the
    /// monitor's next publish.
    ///
    /// A refusal worth stating is stated on `stderr` with the kernel's own
    /// reason rather than a guess — `WouldBlock` is back-pressure from a
    /// mailbox the monitor has not drained, while `NotFound` is an instance
    /// that has exited or has not bound its mailbox yet, and calling the
    /// second one "full" would send a reader looking for a problem that is
    /// not there.
    struct RtSwitchboardMailbox;

    impl SwitchboardMailbox for RtSwitchboardMailbox {
        fn send(&mut self, pid: u64, command: SwitchboardCommand) -> bool {
            let Err(refused) =
                tairix_rt::ipc_send(command_endpoint_for(pid), &command.to_le_bytes())
            else {
                return true;
            };
            if drop_is_noteworthy(command) {
                app::report(
                    APP_NAME,
                    format_args!("switchboard command dropped: {refused}"),
                );
            }
            false
        }
    }

    /// The production [`FrameStatsSink`]: a [`tairix_rt::submit::Submission`]
    /// to the System Information API, handed over on the frame path and
    /// collected on a later pass.
    ///
    /// It never waits: this sits at the end of the compositor's own wake, so a
    /// round trip here is a stall the user sees. A refused submission is
    /// surfaced on `stderr` with the service's own reason rather than a guess,
    /// and dropped: the accounting it carried is cumulative, so the next
    /// attempt states a superset of it and nothing is lost by not retrying.
    struct RtFrameStatsSink {
        totals: tairix_rt::submit::Submission,
    }

    impl RtFrameStatsSink {
        fn new() -> Self {
            Self {
                totals: tairix_rt::submit::Submission::new(MIN_FRAME_PUBLISH_INTERVAL_NS),
            }
        }
    }

    impl FrameStatsSink for RtFrameStatsSink {
        fn submit(&mut self, request: &[u8]) -> Result<(), Errno> {
            match self.totals.post(request) {
                Ok(()) => Ok(()),
                Err(err) => {
                    app::report(
                        APP_NAME,
                        format_args!("frame accounting not handed over: {err}"),
                    );
                    Err(err)
                }
            }
        }

        fn settle(&mut self) -> Option<Result<(), Errno>> {
            let outcome = self.totals.settle()?;
            if let Err(err) = outcome {
                app::report(
                    APP_NAME,
                    format_args!("frame accounting not published: {err}"),
                );
            }
            Some(outcome)
        }
    }

    /// Start the desktop's Switchboard monitor as this logged-in user and
    /// record it in the launch table like any other desktop child,
    /// answering with the pid of the instance now live.
    ///
    /// An instance already recorded is that instance: the monitor's manifest
    /// declares it single-instance like any other bundle, so the one launch
    /// funnel is what keeps a second from starting — this bundle has no rule
    /// of its own. The kernel intersects the monitor's manifest with the
    /// user's ceiling, so its view follows the seat user's authority. A
    /// refused spawn answers `None` and leaves the capsule calm: the desktop
    /// runs without its monitor rather than failing over it.
    fn spawn_switchboard(
        launch: &mut LaunchCtx<'_>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<u64> {
        launch.launch(
            shell,
            compositor,
            SWITCHBOARD_RUN_PATH,
            SWITCHBOARD_LABEL,
            &[],
            None,
        )
    }

    /// Start the desktop's file manager in its **core** role as this
    /// logged-in user and record it in the launch table like any other
    /// desktop child.
    ///
    /// The file manager is a component of the desktop rather than an
    /// application the user starts: it holds a permanent icon-bar slot from
    /// bring-up, offers the user's places and the mounted volumes on that
    /// slot's menu, and never quits. The shared
    /// [`DESKTOP_ROLE_SWITCH`](tairix_window::DESKTOP_ROLE_SWITCH) is what
    /// says so, and the file manager refuses to run without it, so it never
    /// runs as an ordinary application.
    ///
    /// It is spawned before anything else so it takes the leading application
    /// slot: the strip keeps the order the session first saw each process in,
    /// which puts the desktop's own component ahead of whatever the user
    /// starts. A refused spawn is reported by the reap like any other and
    /// leaves the desktop running without it.
    ///
    /// Spawned outright rather than through the launch funnel: at bring-up the
    /// launch table is empty and there is nothing for the funnel to find, and
    /// once it has ended there is no instance left to reach.
    fn spawn_files(launched: &mut LaunchTable) {
        let _ = spawn_and_record(
            launched,
            FILES_RUN_PATH,
            FILES_LABEL,
            &[tairix_window::DESKTOP_ROLE_SWITCH.as_bytes()],
        );
    }

    /// Classify the served presents drained since the last report decision:
    /// Switchboard-only content is what must not re-excite a frame report.
    fn frame_content(
        windows: &mut SessionWindows,
        server: &WindowServer<RtShmMapper>,
        identity: &RtWindowIdentity,
        switchboard_pid: Option<u64>,
    ) -> FrameContent {
        let mut owners = PresentedOwners::default();
        for ipc in windows.take_presented() {
            let pid = server
                .owner_of(ipc)
                .and_then(|client| identity.pid_of(client));
            owners.note(pid, switchboard_pid);
        }
        owners.content()
    }

    /// Name up to [`SEAT_REPORT_OWNERS_MAX`] of the currently-unresponsive
    /// window owners into `owners`, answering how many were named.
    ///
    /// The tracker keys its verdicts by each owner's event-mailbox
    /// endpoint, so an owner is named by matching that endpoint forward
    /// against `event_endpoint_for` of every live window owner — the same
    /// attested ownership records the rest of the session resolves owners
    /// through, never a claimed id or an inverse guessed from the endpoint
    /// number. A flagged owner whose windows have since gone simply goes
    /// unnamed; the report's total still counts it, so the monitor is told
    /// the truth either way.
    fn seat_report_owners(
        sink: &RtEventSink,
        server: &WindowServer<RtShmMapper>,
        windows: &SessionWindows,
        identity: &RtWindowIdentity,
        owners: &mut [u64; SEAT_REPORT_OWNERS_MAX],
    ) -> usize {
        let mut named = 0;
        for endpoint in sink.unresponsive_endpoints() {
            if named == owners.len() {
                break;
            }
            let owner = windows.served().find_map(|(ipc, _)| {
                let client = server.owner_of(ipc)?;
                let pid = identity.pid_of(client)?;
                (event_endpoint_for(pid) == endpoint).then_some(pid)
            });
            if let Some(pid) = owner {
                owners[named] = pid;
                named += 1;
            }
        }
        named
    }

    /// Stops the session's worker threads on every way out of [`session`], so
    /// none is left reading a directory or decoding a picture for a desktop that
    /// has ended.
    ///
    /// The handles are *detached* rather than joined: a worker mid-read of a slow
    /// disk would otherwise hold the teardown for as long as that disk takes, and
    /// it has nothing left to write to — each leaves at its next turn round its
    /// loop, and the desk it shares is kept alive by its own handle until then.
    struct WorkerGuard {
        listings: alloc::sync::Arc<Listings>,
        wallpapers: alloc::sync::Arc<Wallpapers>,
        artworks: alloc::sync::Arc<Artworks>,
        publisher: alloc::sync::Arc<Publisher>,
        catalogs: alloc::sync::Arc<Catalogs>,
    }

    impl Drop for WorkerGuard {
        fn drop(&mut self) {
            self.listings.stop();
            self.wallpapers.stop();
            self.artworks.stop();
            self.publisher.stop();
            self.catalogs.stop();
        }
    }

    /// The handles of the workers this session started, one per kind of work;
    /// the readers its listings, file calls and artwork run on come and go as
    /// the volumes they touch need them, detached.
    ///
    /// Held for the session's life and detached at teardown; a `None` is a
    /// thread the kernel would not grant, whose desk is stopped so its work
    /// falls back to the serve loop.
    #[derive(Default)]
    struct SessionWorkers {
        /// One wallpaper preparer per CPU; empty when the kernel granted none.
        wallpaper: Vec<tairix_rt::thread::JoinHandle<()>>,
        publish: Option<tairix_rt::thread::JoinHandle<()>>,
        catalog: Option<tairix_rt::thread::JoinHandle<()>>,
    }

    /// Spawn one named session worker, stating a refusal once.
    ///
    /// A kernel that will not grant the thread is not a failure: the work moves
    /// back onto the serve loop, which is exactly where it used to be.
    fn spawn_worker(
        what: &str,
        body: impl FnOnce() + Send + 'static,
    ) -> Option<tairix_rt::thread::JoinHandle<()>> {
        match tairix_rt::thread::Thread::spawn(body) {
            Ok(handle) => Some(handle),
            Err(err) => {
                app::report(
                    APP_NAME,
                    format_args!("no {what} thread ({err:?}); that work runs on the serve loop"),
                );
                None
            }
        }
    }

    /// Spawn one wallpaper preparer per `online` CPU, stating once how far
    /// short of that the kernel left it.
    ///
    /// With none the desk is stopped and the backdrop is prepared on the serve
    /// loop; with fewer, previews simply render fewer at once.
    fn spawn_preparers(
        wallpapers: &alloc::sync::Arc<Wallpapers>,
        online: usize,
    ) -> Vec<tairix_rt::thread::JoinHandle<()>> {
        let mut preparers = Vec::with_capacity(online);
        for _ in 0..online {
            let served = alloc::sync::Arc::clone(wallpapers);
            match tairix_rt::thread::Thread::spawn(move || served.serve()) {
                Ok(handle) => preparers.push(handle),
                Err(err) => {
                    let fallback = if preparers.is_empty() {
                        "; the wallpaper is prepared on the serve loop"
                    } else {
                        ""
                    };
                    app::report(
                        APP_NAME,
                        format_args!(
                            "{} of {online} wallpaper preparers ({err:?}){fallback}",
                            preparers.len()
                        ),
                    );
                    break;
                }
            }
        }
        preparers
    }

    /// One frame's bytes in `mode`, checked, so a hostile or corrupt mode can
    /// never size a short region; what the desktop's cache budgets are
    /// derived from too.
    fn frame_len(mode: &DisplayMode) -> Option<usize> {
        u64::from(mode.stride_bytes)
            .checked_mul(u64::from(mode.height_px))
            .and_then(|bytes| usize::try_from(bytes).ok())
            .filter(|bytes| *bytes != 0)
    }

    /// The taskbar layout for an output of this mode.
    ///
    /// One definition, read by the first bring-up and by a resume onto a
    /// screen the next account re-moded, so the bar cannot come back laid
    /// out differently from how it started.
    fn bar_config(screen: Rect) -> TaskbarConfig {
        TaskbarConfig::bottom_bar(screen.width, screen.height)
    }

    /// Lay the bar, the backdrop and its icons out over the screen the
    /// compositor now draws, and put them on it.
    fn lay_out_screen<S: DirectorySource>(
        compositor: &mut Compositor,
        shell: &mut DesktopShell,
        desktop: &mut Desktop<S>,
        pinboard: &mut PinboardPanel,
        wallpapers: &Wallpapers,
    ) {
        shell.set_output_layout(bar_config(compositor.screen_rect()), compositor);
        prepare_wallpaper(
            pinboard,
            wallpapers,
            shell,
            desktop,
            compositor,
            tairix_rt::clock_get(),
        );
        // The new work area may show icons the old one did not; the whole
        // layer is presented next, so no cell needs marking.
        let shown = shell.desktop_layout(compositor, desktop);
        desktop.resolve_occupancy(&shown, &mut Region::new());
        shell.present_desktop(compositor, desktop);
    }

    /// Everything a change to the screen re-lays: the desktop's own surfaces,
    /// the windows on it, and what stands over them.
    struct Resize<'a, S: DirectorySource, C, T> {
        compositor: &'a mut Compositor,
        shell: &'a mut DesktopShell,
        desktop: &'a mut Desktop<S>,
        pinboard: &'a mut PinboardPanel,
        wallpapers: &'a Wallpapers,
        windows: &'a mut SessionWindows,
        menu: &'a mut MenuChain,
        pointer: &'a mut DeviceInputSource<C, T>,
        prompts: (
            &'a mut ConfirmPrompt,
            &'a mut AuthorisePrompt,
            &'a mut ResolutionPrompt,
        ),
        saver: &'a mut DesktopSaver,
        lock: &'a mut ScreenLock,
        idle: &'a mut IdleClock,
        /// The machine's memory, which the frosted backdrops' share is of.
        memory_bytes: usize,
    }

    /// Say that the desktop could not be drawn at `size`, so the one before it
    /// stands.
    fn report_undrawable(size: (u32, u32)) {
        app::report(
            APP_NAME,
            format_args!("the desktop could not be drawn at {}x{}", size.0, size.1),
        );
    }

    /// What drawing the desktop at a size came to.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Resized {
        /// Drawn at the new size; the windows are owed what changed.
        Changed,
        /// Already drawn at it.
        Unchanged,
        /// The size could not be drawn, and the one before it stands.
        Refused,
    }

    /// Draw the desktop at `size` on its output and re-lay the screen for it.
    /// The caller delivers what the windows are owed. Never called while the
    /// screen is locked or a screensaver is up, whose surfaces cover the size
    /// they were drawn at.
    fn resize_desktop<S: DirectorySource, C, T>(
        size: (u32, u32),
        parts: &mut Resize<'_, S, C, T>,
    ) -> Resized {
        let before = parts.compositor.screen_rect();
        if !parts.compositor.set_desktop_size(size.0, size.1) {
            report_undrawable(size);
            return Resized::Refused;
        }
        if parts.compositor.screen_rect() == before {
            return Resized::Unchanged;
        }
        relay_screen(parts);
        Resized::Changed
    }

    /// Draw the desktop on an output running `mode`, at the resolution the
    /// settings name where it offers it, and re-lay the screen for it. A
    /// resolution trial open over the output that is gone is declined.
    ///
    /// Answers `false`, changing nothing, for a mode the compositor cannot
    /// draw. The caller delivers what the windows are owed.
    fn adopt_output<S: DirectorySource, C, T>(
        mode: DisplayMode,
        parts: &mut Resize<'_, S, C, T>,
    ) -> bool {
        if !parts.compositor.set_mode(mode) {
            return false;
        }
        // The caches a screenful bounds are held to the screen now drawn.
        if let Some(fb_bytes) = frame_len(&mode) {
            parts
                .compositor
                .rescale_caches(fb_bytes, parts.memory_bytes);
            parts.shell.rescale_caches(fb_bytes);
        }
        let _ = parts.prompts.2.abandon(parts.shell, parts.compositor);
        let size = parts
            .desktop
            .settings()
            .resolution
            .on((mode.width_px, mode.height_px));
        if !parts.compositor.set_desktop_size(size.0, size.1) {
            report_undrawable(size);
        }
        relay_screen(parts);
        true
    }

    /// Re-lay everything drawn to the screen's size over the screen the
    /// compositor draws now: the bar, the backdrop and its icons, the windows
    /// fitted onto it, the prompts placed on it again, any menu or tip and the
    /// screensaver taken down — the screensaver due again at once, over the
    /// new screen — the pointer held on it, and every application told. A
    /// lock up is drawn over the new screen here, before anything presents
    /// it; one it cannot be drawn over leaves the screen unpresented.
    fn relay_screen<S: DirectorySource, C, T>(parts: &mut Resize<'_, S, C, T>) {
        let screen = parts.compositor.screen_rect();
        lay_out_screen(
            parts.compositor,
            parts.shell,
            parts.desktop,
            parts.pinboard,
            parts.wallpapers,
        );
        let work_area = parts.shell.work_area(parts.compositor);
        parts.windows.refit(parts.compositor, work_area);
        if parts.menu.dismiss() {
            present_menu_chain(parts.menu, parts.shell, parts.compositor, parts.windows);
        }
        if parts.shell.dismiss_tooltip() {
            let _ = parts.shell.present_tooltip(parts.compositor);
        }
        let (confirm, elevate, resolution) = &mut parts.prompts;
        confirm.repaint(parts.shell, parts.compositor);
        elevate.repaint(parts.shell, parts.compositor);
        resolution.repaint(parts.shell, parts.compositor);
        parts.windows.alerts.repaint(parts.shell, parts.compositor);
        if parts.saver.dismiss(parts.compositor, None) == Ok(true) {
            parts.wallpapers.forget_slides();
            parts.idle.saver_withdrawn();
        }
        parts
            .lock
            .keep_topmost(parts.shell, parts.compositor, parts.saver.window());
        if let Ok(at) = parts.pointer.set_screen(screen, parts.compositor.scale()) {
            parts.shell.track_pointer(at, parts.compositor);
        }
        publish_desktop(parts.compositor, parts.shell);
        parts.shell.note_resized();
    }

    /// A pinboard call asking for a resolution other than the one drawn: the
    /// size it asks for, and the trial that holds the call.
    struct TrialAsk {
        size: (u32, u32),
        trial: Trial,
    }

    /// Draw the size `ask` names and put it to the user. One that cannot be
    /// drawn is refused at once, and one that cannot be asked is answered
    /// declined; the resolution in force is drawn again either way by the
    /// convergence that follows.
    fn begin_trial<S: DirectorySource, C, T>(
        ask: TrialAsk,
        parts: &mut Resize<'_, S, C, T>,
    ) -> Option<TrialPlacements> {
        let TrialAsk { size, trial } = ask;
        let before = parts.compositor.placements();
        if resize_desktop(size, parts) == Resized::Refused {
            reply_pinboard(trial.ticket, Err(Errno::OutOfMemory));
            return None;
        }
        let placements = TrialPlacements {
            before,
            after: parts.compositor.placements(),
        };
        match parts
            .prompts
            .2
            .ask(trial, tairix_rt::clock_get(), parts.shell, parts.compositor)
        {
            Ok(()) => log_info(TRIAL_SHOWN, TRIAL_SHOWN_MESSAGE, &[]),
            Err(trial) => {
                io::write_stderr_line(
                    "desktop: the new resolution could not be put to you, so it was not kept",
                );
                reply_pinboard(trial.ticket, Err(Errno::Declined));
            }
        }
        Some(placements)
    }

    /// Where the windows sat before a trial's size was drawn and where its
    /// refit left them, so a reverted trial puts back what the refit moved.
    struct TrialPlacements {
        before: Vec<tairix_wm::Placement>,
        after: Vec<tairix_wm::Placement>,
    }

    /// Put a trial's end into effect. A kept resolution's request is merged
    /// over the settings in force and persisted, its call answered once the
    /// store has spoken and the size held on screen until then; a reverted
    /// one's call is answered declined.
    fn conclude_trial<S: DirectorySource>(
        end: TrialEnd,
        publisher: &Publisher,
        pinboard: &mut PinboardPanel,
        wallpapers: &Wallpapers,
        desktop: &mut Desktop<S>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        match end {
            TrialEnd::Reverted(trial) => {
                reply_pinboard(trial.ticket, Err(Errno::Declined));
                log_info(
                    TRIAL_CONCLUDED,
                    TRIAL_CONCLUDED_MESSAGE,
                    &TrialOutcome::Reverted.fields(),
                );
            }
            TrialEnd::Kept(trial) => {
                match merge_applied(&publisher.base(desktop.settings()), &trial.request) {
                    Ok(settings) => {
                        publisher.keep(trial.ticket);
                        if request_pinboard_settings(
                            settings,
                            publisher,
                            Some(trial.ticket),
                            pinboard,
                            wallpapers,
                            desktop,
                            shell,
                            compositor,
                            tairix_rt::clock_get(),
                        ) {
                            shell.present_desktop(compositor, desktop);
                        }
                    }
                    Err(refusal) => {
                        let refusal = PinboardRefusal::Undecodable(refusal);
                        app::report(APP_NAME, format_args!("{}", refusal.reason()));
                        reply_pinboard(trial.ticket, Err(refusal.errno()));
                        log_info(
                            TRIAL_CONCLUDED,
                            TRIAL_CONCLUDED_MESSAGE,
                            &TrialOutcome::Refused.fields(),
                        );
                    }
                }
            }
        }
    }

    /// Take the frames the display allocated for `mode` the one way every
    /// presenter does (`tairix_display::establish_display`).
    fn establish_frames(mode: &DisplayMode) -> Result<Ring, FramesRefused> {
        tairix_display::establish_display(
            DisplayClient::new(RtDisplayTransport, SEAT_PRIMARY),
            *mode,
        )
    }

    /// Hold `ring` as the session's display in place of whatever it held,
    /// its latch port waited on in `set` in place of the last one's.
    ///
    /// # Errors
    ///
    /// The wait-set's refusal of the port, which leaves the session holding
    /// no display: a present the device queued would never be heard to
    /// latch.
    fn hold_display(set: u64, display: &mut Option<Ring>, ring: Option<Ring>) -> Result<(), Errno> {
        if let Some(old) = display.take() {
            let _ = tairix_rt::waitset_ctl(
                set,
                WaitSetOp::Del,
                WaitSourceKind::Port,
                old.events_port(),
                DISPLAY_EVENT_TOKEN,
            );
        }
        let Some(ring) = ring else {
            return Ok(());
        };
        let added = tairix_rt::waitset_ctl(
            set,
            WaitSetOp::Add,
            WaitSourceKind::Port,
            ring.events_port(),
            DISPLAY_EVENT_TOKEN,
        );
        if added != 0 {
            return Err(Errno::from_syscall(added));
        }
        *display = Some(ring);
        Ok(())
    }

    /// The exit code a bring-up whose frames were refused ends with.
    const fn refused_exit(refused: FramesRefused) -> i32 {
        match refused {
            FramesRefused::Configure(Errno::LengthOutOfRange) => EXIT_BAD_MODE,
            FramesRefused::Port(_) => EXIT_NO_FRAMES,
            FramesRefused::Configure(_) => EXIT_NO_DISPLAY,
        }
    }

    /// The step-aside request over the reserved session rendezvous.
    ///
    /// The frame is bodyless and carries no identity: the authority attests
    /// the caller from the kernel and honours the request only from the
    /// session it records as the foreground one.
    struct RtSessionAuthority;

    impl SessionAuthority for RtSessionAuthority {
        fn request_background(&mut self) -> Result<SessionVerdict, Errno> {
            let mut request = [0u8; SESSION_MAX_REQUEST];
            let len = SessionRequest::Background.encode(&mut request)?;
            let mut reply = [0u8; SESSION_VERDICT_LEN];
            let got = tairix_rt::ipc_call(SESSION_ENDPOINT, &request[..len], &mut reply)
                .map_err(Errno::from_syscall)?;
            SessionVerdict::decode(&reply[..got])
        }
    }

    /// The session giving its screen up, as the switch drives it: the frame
    /// ring, the compositor the fade is drawn through, and the wait-set the
    /// seat member belongs to.
    struct SessionDeparture<'a> {
        display: &'a mut Option<Ring>,
        compositor: &'a mut Compositor,
        fade: &'a mut ScreenFade,
        set: u64,
    }

    impl SeatDeparture for SessionDeparture<'_> {
        fn fade_out(&mut self) {
            fade_to_black(self.fade, self.compositor, self.display);
        }

        fn suspend(&mut self) {
            // The ring goes with its frames, and the seat's wait-set member
            // goes with them: a parked session must not be woken by the next
            // account's typing, and ignoring such a wake instead would spin on
            // a member that stays ready.
            let _ = hold_display(self.set, self.display, None);
            let _ = tairix_rt::waitset_ctl(
                self.set,
                WaitSetOp::Del,
                WaitSourceKind::SeatInput,
                SEAT_PRIMARY,
                SEAT_TOKEN,
            );
        }

        fn release_seat(&mut self) {
            // The login screen is what comes up next, so the seat is handed
            // on cleared: this account's last frame must not linger on the
            // screen for the next person, and no text console belongs in the
            // gap either.
            let _ = tairix_rt::display_release(SEAT_PRIMARY, ReleaseSurface::Handover);
        }
    }

    /// The session taking its screen back, as the switch drives it: the
    /// frame ring, the fade, the wait-set the seat member joins, and
    /// everything the screen re-lays.
    struct SessionReturn<'a, 'p, S: DirectorySource, C, T> {
        display: &'a mut Option<Ring>,
        fade: &'a mut ScreenFade,
        set: u64,
        parts: Resize<'p, S, C, T>,
    }

    impl<S: DirectorySource, C, T> SeatReturn for SessionReturn<'_, '_, S, C, T> {
        fn acquire_seat(&mut self) -> Result<(), Errno> {
            let taken = tairix_rt::display_acquire(SEAT_PRIMARY);
            if taken < 1 {
                return Err(Errno::from_syscall(taken));
            }
            if tairix_rt::waitset_ctl(
                self.set,
                WaitSetOp::Add,
                WaitSourceKind::SeatInput,
                SEAT_PRIMARY,
                SEAT_TOKEN,
            ) != 0
            {
                let _ = tairix_rt::display_release(SEAT_PRIMARY, ReleaseSurface::Text);
                return Err(Errno::SeatRevoked);
            }
            Ok(())
        }

        fn query_output(&mut self) -> Result<Option<OutputMode>, Errno> {
            DisplayClient::new(RtDisplayTransport, SEAT_PRIMARY).query()
        }

        fn reconfigure(&mut self, OutputMode { mode, refresh }: OutputMode) -> Result<(), Errno> {
            self.parts.compositor.set_refresh_rate(refresh);
            let ring = match establish_frames(&mode) {
                Ok(ring) => ring,
                Err(FramesRefused::Configure(Errno::Stale)) => return Err(Errno::Stale),
                Err(_) => return Err(Errno::DeviceFault),
            };
            hold_display(self.set, self.display, Some(ring)).map_err(|_| Errno::DeviceFault)?;
            // The compositor adopts the mode before anything is laid out
            // against it; a mode it cannot take leaves it untouched, and the
            // session ends rather than showing a screen it cannot draw.
            if adopt_output(mode, &mut self.parts) {
                Ok(())
            } else {
                Err(Errno::NotSupported)
            }
        }

        fn publish_desktop(&mut self) {
            publish_desktop(self.parts.compositor, self.parts.shell);
        }

        fn fade_in(&mut self) {
            self.fade
                .arrive(tairix_rt::clock_get(), self.parts.compositor);
        }

        fn repaint_all(&mut self, _mode: DisplayMode) -> Result<(), Errno> {
            let Some(display) = self.display.as_mut() else {
                return Err(Errno::NotConnected);
            };
            // The loop presents once the lock covers the screen again.
            if self.parts.lock.uncovered(self.parts.compositor) {
                return Ok(());
            }
            self.parts
                .compositor
                .present(display)
                .map_err(|_| Errno::DeviceFault)
        }
    }

    /// What following the output came to.
    enum Followed {
        /// Nothing the windows are owed: the output is gone, the frames
        /// still serve it, or new ones could not be had.
        Settled,
        /// The screen was re-laid for the output's new mode; the windows are
        /// owed what changed.
        Moved(DisplayMode),
    }

    /// Converge on the display's output once (`plans/DISPLAY.md` D12).
    ///
    /// `forgotten` says the service refused the frames as unknown since the
    /// last look. With no output the frames are given back and nothing is
    /// composed; with the mode the frames hold, the whole output is owed to
    /// the display again; with any other, frames are established for it and
    /// the screen re-laid. A refusal leaves the session dormant with its
    /// reason stated, and the next announcement tries again.
    ///
    /// # Errors
    ///
    /// The exit code a lost seat ends the session with.
    fn follow_output<S: DirectorySource, C, T>(
        forgotten: bool,
        (display, set): (&mut Option<Ring>, u64),
        parts: &mut Resize<'_, S, C, T>,
    ) -> Result<Followed, i32> {
        let output = match DisplayClient::new(RtDisplayTransport, SEAT_PRIMARY).query() {
            Ok(output) => output,
            Err(Errno::SeatRevoked | Errno::SeatNotOwner | Errno::PermissionDenied) => {
                parts.shell.teardown(parts.compositor);
                return Err(app::fail(
                    APP_NAME,
                    EXIT_SEAT_LOST,
                    "seat lease lost; tearing the session down",
                ));
            }
            Err(err) => {
                app::report(
                    APP_NAME,
                    format_args!("the display output could not be read ({err:?})"),
                );
                return Ok(Followed::Settled);
            }
        };
        let held = display.as_ref().map(Presenter::mode);
        // A present still waiting to latch when the output is announced again
        // went to a driver that may not be there to latch it.
        let forgotten = forgotten || display.as_ref().is_some_and(|ring| !ring.ready());
        let rate_moved = parts
            .compositor
            .set_refresh_rate(output.and_then(|output| output.refresh));
        match Follow::of(output.map(|output| output.mode), held, forgotten) {
            Follow::Release => {
                if display.is_some() {
                    let _ = hold_display(set, display, None);
                    io::write_stderr_line("desktop: no display output is connected");
                }
                Ok(Followed::Settled)
            }
            Follow::Repaint => {
                parts.compositor.owe_output();
                // A new mode tells every application through the re-lay; the
                // same mode at a new rate is told here.
                if rate_moved {
                    publish_desktop(parts.compositor, parts.shell);
                }
                Ok(Followed::Settled)
            }
            Follow::Reconfigure(mode) => {
                let _ = hold_display(set, display, None);
                match establish_frames(&mode) {
                    Ok(ring) => {
                        if let Err(err) = hold_display(set, display, Some(ring)) {
                            app::report(
                                APP_NAME,
                                format_args!("the display's latches cannot be waited on ({err:?})"),
                            );
                            return Ok(Followed::Settled);
                        }
                    }
                    // It moved again, and that announcement brings the
                    // session onto where it went.
                    Err(FramesRefused::Configure(Errno::Stale)) => return Ok(Followed::Settled),
                    Err(refused) => {
                        app::report(APP_NAME, refused.reason());
                        return Ok(Followed::Settled);
                    }
                }
                if !adopt_output(mode, parts) {
                    let _ = hold_display(set, display, None);
                    app::report(
                        APP_NAME,
                        format_args!(
                            "the display's new mode {}x{} cannot be drawn",
                            mode.width_px, mode.height_px
                        ),
                    );
                    return Ok(Followed::Settled);
                }
                Ok(Followed::Moved(mode))
            }
        }
    }

    /// The output the session comes up on: the one connected now, or the
    /// first to arrive, parked for on `set`, which already holds the output's
    /// announcement so none is missed between a read and the park.
    fn first_output(set: u64) -> Result<OutputMode, (i32, &'static str)> {
        let mut client = DisplayClient::new(RtDisplayTransport, SEAT_PRIMARY);
        let mut told = false;
        loop {
            match client.query() {
                Ok(Some(mode)) => return Ok(mode),
                Ok(None) => {}
                Err(_) => {
                    return Err((
                        EXIT_NO_DISPLAY,
                        "display service unreachable or refused the output query",
                    ));
                }
            }
            if !told {
                told = true;
                io::write_stderr_line("desktop: no display output is connected; waiting for one");
            }
            let mut token = 0u64;
            if tairix_rt::waitset_wait(set, WAITSET_TIMEOUT_NONE, &mut token) != 0 {
                return Err((EXIT_WAIT_FAILED, "display output wait failed"));
            }
        }
    }

    /// Bring the desktop up and run it until the seat is lost or a fault
    /// ends it. Split from `main` so every exit path after the acquire
    /// flows back through the one owner-checked `display_release`.
    #[allow(clippy::too_many_lines)] // One linear bring-up + serve loop; splitting it would scatter the lease lifecycle.
    fn session() -> i32 {
        // The session's own kernel-attested identity, read before anything
        // else: the window engine stamps it into every create reply, the
        // wake mailbox below is addressed by its pid, and a session that
        // cannot learn who it is must not serve windows apps cannot
        // authenticate (fail closed).
        let Ok(self_origin) = tairix_rt::self_origin() else {
            return app::fail(
                APP_NAME,
                EXIT_NO_WINDOW_ENDPOINT,
                "session identity unavailable",
            );
        };
        // Bind the fast-user-switching wake mailbox before the first frame,
        // so a session is resumable from the moment it can be switched away
        // from. The id is derived from this session's own pid, so only the
        // session authority is admitted to it — no one else can fill it —
        // and every message is still attested when it is drained. A refused
        // bind or admission is not fatal: the desktop runs as a session that
        // simply cannot be switched away from, and says so by leaving the
        // row out.
        let wake = session_wake_endpoint(self_origin.pid());
        let bound = !tairix_abi::ipc::is_reserved_endpoint(wake)
            && tairix_rt::port_bind(wake, SESSION_WAKE_LEN, WAKE_DEPTH) == 0
            && tairix_rt::port_admit(wake, SESSION_ENDPOINT).is_ok();
        if !bound {
            io::write_stderr_line(
                "desktop: session wake mailbox refused; this session cannot switch user",
            );
        }
        let mut switch = SwitchUser::new(bound.then_some(wake), self_origin.console());

        // The wait-set comes first, holding the output's announcement before
        // the output is read, so a move between the read and the join cannot
        // be missed. Made before any listing is asked for, so every watch a
        // listing arms joins the set the moment it is taken.
        let set = tairix_rt::waitset_create();
        if set < 0 {
            return app::fail(APP_NAME, EXIT_WAIT_FAILED, "wait-set refused");
        }
        #[allow(clippy::cast_sign_loss)] // `set >= 0` checked above; it is a kernel handle.
        let set = set as u64;
        if tairix_rt::waitset_ctl(
            set,
            WaitSetOp::Add,
            WaitSourceKind::SystemNotice,
            u64::from(NoticeTopic::DisplayOutput.as_u32()),
            DISPLAY_OUTPUT_TOKEN,
        ) != 0
        {
            return app::fail(APP_NAME, EXIT_WAIT_FAILED, "display output wait refused");
        }

        // --- Display bring-up: the output → shared frames → grant →
        // configure. A configure refused as stale means the output moved
        // after it was read, and its announcement is what is waited for.
        let mut moved = false;
        let (ring, OutputMode { mode, refresh }) = loop {
            if moved {
                let mut token = 0u64;
                if tairix_rt::waitset_wait(set, WAITSET_TIMEOUT_NONE, &mut token) != 0 {
                    return app::fail(APP_NAME, EXIT_WAIT_FAILED, "display output wait failed");
                }
            }
            let output = match first_output(set) {
                Ok(output) => output,
                Err((code, reason)) => return app::fail(APP_NAME, code, reason),
            };
            match establish_frames(&output.mode) {
                Ok(ring) => break (ring, output),
                Err(FramesRefused::Configure(Errno::Stale)) => moved = true,
                Err(refused) => {
                    return app::fail(APP_NAME, refused_exit(refused), refused.reason());
                }
            }
        };
        let Some(frame_len) = frame_len(&mode) else {
            return app::fail(APP_NAME, EXIT_BAD_MODE, "queried mode is zero-sized");
        };
        // Held as an option so a step-aside, or an output that went away, can
        // give the frames back and a resume or a new output build them again
        // for the mode then in force — there is no second bring-up path.
        let mut display = None;
        if hold_display(set, &mut display, Some(ring)).is_err() {
            return app::fail(APP_NAME, EXIT_WAIT_FAILED, "display latch wait refused");
        }
        // Set while the service refuses the frames, until its next
        // announcement of the output: whether it holds no configuration for
        // them at all.
        let mut suspended: Option<bool> = None;

        // --- Desktop bring-up: the shell, the compositor over the active
        // theme's desktop colour, and the two live seat input sources with
        // the queried mode as the pointer's screen rectangle.
        // The seat's rasterised-asset caches are budgeted from one frame of
        // this very output, so the desktop is allowed more cached pixels on
        // a large display than a small one and no ceiling is guessed. They
        // are governed by the process pressure gauge, which the wait loop
        // below keeps current from the kernel's band.
        //
        // Publish the band *before* those caches exist: the gauge starts in
        // its fail-closed unknown state, where every cache admits nothing, so
        // a desktop that waited for its wait-set member would draw the whole
        // bring-up with no cached cursor, glyph, or icon artwork.
        let _ = tairix_procinfo::pressure::refresh();
        // The machine's memory bounds what one client may map here, what a
        // ray-traced scene may hold, and how much decoded artwork and stacked
        // frosted glass is retained.
        let memory_read = tairix_procinfo::memory_total_bytes(&IpcTransport).ok();
        let memory_total = memory_read.unwrap_or(0);
        let memory_bytes = usize::try_from(memory_total).unwrap_or(usize::MAX);
        let mut shell = DesktopShell::new(
            bar_config(Rect::new(0, 0, mode.width_px, mode.height_px)),
            SEAT_PRIMARY,
            (frame_len, memory_bytes),
            tairix_rt::pressure::gauge(),
            &LOG_SINK,
        );
        // The shell registered the desktop's own cache rows, so from here
        // every way out — a bring-up refusal below or the serve loop's
        // fail-loud exit — has to take them back out of the monitor's
        // registry; a dropped guard does that once, unconditionally.
        let _cache_report = tairix_rt::cachereport::ReportGuard;
        // The decorated windows' furniture and the backdrop-blurred windows'
        // frosted backdrops are the output's own caches, so they are built
        // here from the same seat, output size, gauge, and sink and handed to
        // the compositor that draws from them. The compositor takes the gauge
        // itself as well: a window's *content* is not a keyed cache but a
        // release policy over the same band, so it reads the pressure directly
        // rather than through a cache.
        //
        // They are this process's memory like the shell's three caches, so
        // they join them in the report before the compositor takes them: a
        // ledger is a shared handle to the figures, not the cache itself.
        let chrome = chrome_cache(
            SEAT_PRIMARY,
            frame_len,
            tairix_rt::pressure::gauge(),
            &LOG_SINK,
        );
        let frost = frost_cache(
            SEAT_PRIMARY,
            frame_len,
            memory_bytes,
            tairix_rt::pressure::gauge(),
            &LOG_SINK,
        );
        for ledger in [chrome.ledger(), frost.ledger()].into_iter().flatten() {
            tairix_rt::cachereport::register(ledger);
        }
        let Some(mut compositor) = Compositor::new(
            mode,
            shell.session().active_theme().clone(),
            chrome,
            frost,
            tairix_rt::pressure::gauge(),
        ) else {
            return app::fail(
                APP_NAME,
                EXIT_BAD_MODE,
                "compositor rejected the queried mode",
            );
        };
        compositor.set_refresh_rate(refresh);
        let cpus = tairix_procinfo::cpu_info(&IpcTransport).ok();
        let online = tairix_procinfo::online_count(cpus.as_deref());
        compositor.set_job_runner(composite_pool(online));
        let screen = Rect::new(0, 0, mode.width_px, mode.height_px);
        let Ok(mut pointer) = DeviceInputSource::new(
            SeatInputChannel::new(PointerReader),
            SeatInputChannel::new(TouchReader),
            screen,
        ) else {
            return app::fail(
                APP_NAME,
                EXIT_BAD_MODE,
                "queried mode has no pointer surface",
            );
        };
        // Built on the defaults; the loop head reconciles the user's policy
        // into it, and into the pointer, once the settings load.
        let mut input_policy = InputPolicy::of(&DesktopSettings::default());
        drive_pointer(&mut pointer, &input_policy);
        let mut keyboard =
            KeyboardInputSource::new(SeatInputChannel::new(KeyboardReader), input_policy.repeat);

        // The serve loop's own parser-sandbox worker: this binary re-entered as
        // a capability-empty child, where untrusted images are decoded rather
        // than in this address space. The wallpaper and artwork threads own one
        // each of their own, so no sandbox handle crosses a thread; this one is
        // what both fall back to when the kernel grants no thread to hold them.
        let sandbox: SharedSandbox =
            alloc::rc::Rc::new(core::cell::RefCell::new(ParserSandbox::new(
                RtLauncher::own_binary(tairix_sandbox::WorkerMemory::Documents),
                tairix_rt::LogSink,
            )));

        // The session's workers, and the one pipe they all nudge the serve loop
        // through: a directory read, an icon decode, and a wallpaper
        // preparation are each a disk and a sandbox away, so each costs a
        // repaint's delay rather than a frozen desktop. A pipe the kernel
        // refuses, or a thread it will not grant, leaves that work on the serve
        // loop's own task: slower under load, never wrong, and stated once.
        let worker_wake = alloc::sync::Arc::new(tairix_rt::sync::WorkerWake::create());
        if !worker_wake.is_armed() {
            io::write_stderr_line(
                "desktop: no worker wake pipe; directory listings, icon artwork, the wallpaper, \
                 the program catalogue, settings publishing, and opening files all happen on \
                 the serve loop",
            );
        }
        // Every call this session posts to the cue authority — a notification's
        // cue, the volume's feedback, the push of its user's choices — is
        // answered on one wake.
        let mut cue_posts = tairix_soundtheme::PostedCues::new();
        // A user names the paths the desktop's listed bundles are indexed by,
        // so the index is keyed; without a key it is never asked anything.
        let keys = tairix_rt::hash_seed().map(tairix_hash::BuildSipHash13::with_seed);
        if keys.is_none() {
            io::write_stderr_line("desktop: no hash key; icons draw their built-in pictures");
        }
        let artworks = alloc::sync::Arc::new(Artworks::new(
            alloc::sync::Arc::clone(&worker_wake),
            keys.unwrap_or(tairix_hash::BuildSipHash13::UNKEYED),
        ));
        let wallpapers =
            alloc::sync::Arc::new(Wallpapers::new(alloc::sync::Arc::clone(&worker_wake)));
        let listings = alloc::sync::Arc::new(Listings::new(
            alloc::sync::Arc::clone(&worker_wake),
            set,
            (
                alloc::sync::Arc::clone(&artworks),
                alloc::sync::Arc::clone(&wallpapers),
            ),
        ));
        let publisher =
            alloc::sync::Arc::new(Publisher::new(alloc::sync::Arc::clone(&worker_wake)));
        let catalogs = alloc::sync::Arc::new(Catalogs::new(alloc::sync::Arc::clone(&worker_wake)));
        // One worker per kind of work, spawned only where there is a wake to
        // deliver through. Each handle is held for the session's life; the
        // worker's own `Arc` keeps its desk alive either way.
        let workers = if worker_wake.is_armed() {
            SessionWorkers {
                wallpaper: spawn_preparers(&wallpapers, online),
                publish: {
                    let served = alloc::sync::Arc::clone(&publisher);
                    spawn_worker("settings", move || served.serve())
                },
                catalog: {
                    let served = alloc::sync::Arc::clone(&catalogs);
                    spawn_worker("program catalogue", move || served.serve())
                },
            }
        } else {
            SessionWorkers::default()
        };
        // With no worker there is nobody to answer a recorded request, so the
        // desk is stopped and that work happens on this task instead.
        if !(worker_wake.is_armed() && listings.start()) {
            listings.alone();
        }
        if workers.wallpaper.is_empty() {
            wallpapers.stop();
        }
        let preparers = workers.wallpaper.len();
        wallpapers.adopt_band(preparers, memory_total, tairix_rt::pressure::gauge().band());
        // The artwork's readers come and go as the volumes its pictures are on
        // need them; with none the decode happens on this task.
        let artwork_readers = keys.is_some() && worker_wake.is_armed() && artworks.start();
        if artwork_readers {
            artworks.admit(memory_total);
            artworks.adopt_band();
        } else {
            artworks.stop();
        }
        if workers.publish.is_none() {
            publisher.stop();
        }
        if workers.catalog.is_none() {
            catalogs.stop();
        }
        // The audio service's round trips leave the loop, as every other
        // service call does; with no thread they run here.
        let sound_worker = alloc::sync::Arc::new(Worker::new(
            serve_sound,
            RtAudio::new(),
            tairix_rt::sync::WorkerWake::create(),
        ));
        if let Err(reason) = Worker::start(&sound_worker) {
            app::report(
                APP_NAME,
                format_args!("no sound thread ({reason:?}); that work runs on the serve loop"),
            );
        }
        let _sound_guard = tairix_rt::work::WorkerGuard::new(&sound_worker);
        let mut sound = SoundSession::new();
        // The private scope's round trips leave the loop: the sound choices,
        // the files they name and the cue authority, the launcher's record and
        // where the desktop's icons were put. One worker carries all of them,
        // because the app-data service stages edits per process and two
        // writers would commit each other's.
        let private_worker = match Worker::queued(
            serve_private,
            (),
            tairix_rt::sync::WorkerWake::create(),
            PRIVATE_JOBS,
        ) {
            Ok(worker) => alloc::sync::Arc::new(worker),
            Err(_) => {
                return app::fail(
                    APP_NAME,
                    EXIT_WAIT_FAILED,
                    "no memory for the private-store worker",
                );
            }
        };
        if let Err(reason) = Worker::start(&private_worker) {
            app::report(
                APP_NAME,
                format_args!(
                    "no private-store thread ({reason:?}); that work runs on the serve loop"
                ),
            );
        }
        let _private_guard = tairix_rt::work::WorkerGuard::new(&private_worker);
        let mut effects = Effects::new();
        let mut memory = PrivateRecords::new();
        // Each password offered at the screen lock waits on the broker's key
        // derivation away from the loop that composites every window.
        let unlock_worker = alloc::sync::Arc::new(Worker::new(
            ask_broker,
            (),
            tairix_rt::sync::WorkerWake::create(),
        ));
        if let Err(reason) = Worker::start(&unlock_worker) {
            app::report(
                APP_NAME,
                format_args!(
                    "no unlock thread ({reason:?}); the screen lock waits for each answer on the serve loop"
                ),
            );
        }
        let _unlock_guard = tairix_rt::work::WorkerGuard::new(&unlock_worker);
        let mut unlocker = Asking::new(ToBroker(alloc::sync::Arc::clone(&unlock_worker)));
        let elevate_worker = alloc::sync::Arc::new(Worker::new(
            ask_broker_for,
            (),
            tairix_rt::sync::WorkerWake::create(),
        ));
        if let Err(reason) = Worker::start(&elevate_worker) {
            app::report(
                APP_NAME,
                format_args!(
                    "no elevation thread ({reason:?}); the authorisation prompt waits for each answer on the serve loop"
                ),
            );
        }
        let _elevate_guard = tairix_rt::work::WorkerGuard::new(&elevate_worker);
        // The one thread that waits on the font service: the desktop draws
        // without waiting, and this fetches what it drew provisionally. With
        // no thread, text is fetched on the serve loop as it is drawn.
        let font_worker = alloc::sync::Arc::new(Worker::new(
            fetch_fonts,
            (),
            tairix_rt::sync::WorkerWake::create(),
        ));
        match Worker::start(&font_worker) {
            Ok(_) => tairix_font::defer_fetches(),
            Err(reason) => app::report(
                APP_NAME,
                format_args!("no font thread ({reason:?}); text is fetched on the serve loop"),
            ),
        }
        let _font_guard = tairix_rt::work::WorkerGuard::new(&font_worker);
        // A saver's font calls wait on the font service off the loop, on a
        // thread of their own so none waits behind a picture being written.
        let saver_fonts: alloc::sync::Arc<SaverFontWorker> = alloc::sync::Arc::new(Worker::new(
            relay_saver_font,
            alloc::vec::Vec::new(),
            tairix_rt::sync::WorkerWake::create(),
        ));
        if let Err(reason) = Worker::start(&saver_fonts) {
            app::report(
                APP_NAME,
                format_args!(
                    "no saver font thread ({reason:?}); a screensaver's text is fetched on the serve loop"
                ),
            );
        }
        let _saver_fonts_guard = tairix_rt::work::WorkerGuard::new(&saver_fonts);
        // The pictures savers keep are written, and the screensaver stores
        // read, off the loop.
        let saver_home = home_dir();
        let saver_files: alloc::sync::Arc<SaverFileWorker> = match Worker::queued(
            file_for_saver,
            SaverFiles {
                store: watch_saver_store(set, saver_home.as_deref()),
                home: saver_home,
            },
            tairix_rt::sync::WorkerWake::create(),
            SAVER_FILE_JOBS,
        ) {
            Ok(worker) => alloc::sync::Arc::new(worker),
            Err(_) => {
                return app::fail(
                    APP_NAME,
                    EXIT_WAIT_FAILED,
                    "no memory for the screensaver file worker",
                );
            }
        };
        if let Err(reason) = Worker::start(&saver_files) {
            app::report(
                APP_NAME,
                format_args!(
                    "no screensaver file thread ({reason:?}); the screensaver stores are read and \
                     its pictures kept on the serve loop"
                ),
            );
        }
        let _saver_files_guard = tairix_rt::work::WorkerGuard::new(&saver_files);
        // Captures are kept, and recordings' files made and named, off the loop.
        let capture_files: alloc::sync::Arc<CaptureFileWorker> = match Worker::queued(
            file_for_capture,
            CaptureFiles { home: home_dir() },
            tairix_rt::sync::WorkerWake::create(),
            CAPTURE_FILE_JOBS,
        ) {
            Ok(worker) => alloc::sync::Arc::new(worker),
            Err(_) => {
                return app::fail(
                    APP_NAME,
                    EXIT_WAIT_FAILED,
                    "no memory for the capture file worker",
                );
            }
        };
        if let Err(reason) = Worker::start(&capture_files) {
            app::report(
                APP_NAME,
                format_args!(
                    "no capture file thread ({reason:?}); captures are kept on the serve loop"
                ),
            );
        }
        let _capture_files_guard = tairix_rt::work::WorkerGuard::new(&capture_files);
        // Which volumes await the seat's answer for their held writes is read,
        // and each answer carried to the kernel, off the loop.
        let held_writes: alloc::sync::Arc<HeldWorker> = match Worker::queued(
            tairix_desktop_session::held::work::<RtHeld>,
            RtHeld,
            tairix_rt::sync::WorkerWake::create(),
            HELD_JOBS,
        ) {
            Ok(worker) => alloc::sync::Arc::new(worker),
            Err(_) => {
                return app::fail(
                    APP_NAME,
                    EXIT_WAIT_FAILED,
                    "no memory for the held-writes worker",
                );
            }
        };
        if let Err(reason) = Worker::start(&held_writes) {
            app::report(
                APP_NAME,
                format_args!(
                    "no held-writes thread ({reason:?}); held writes are read and answered on \
                     the serve loop"
                ),
            );
        }
        let _held_writes_guard = tairix_rt::work::WorkerGuard::new(&held_writes);
        let mut held_questions = HeldQuestions::default();
        // This session took the seat before it got here.
        effects.seat(true);
        let sound_themes = list_sound_themes();
        // The shipped wallpaper store, walked once: `/System` is read-only,
        // so this is the catalog for the life of the boot and the query that
        // answers it never reaches a directory again. Bring-up, not a frame:
        // a browsing application must never make the compositor walk a
        // store.
        let wallpaper_catalog = list_wallpaper_store();
        // The shipped cursor store, walked once for the same reason, and
        // every set it offers loaded here rather than when one is first
        // chosen: a set becomes active on the loop that owes the user a
        // frame, so activating one must not read a directory.
        let cursor_sets = load_cursor_sets(&shell);
        let cursor_set_names: Vec<tairix_window::CursorSetName> = cursor_sets
            .iter()
            .map(|(id, _)| tairix_window::CursorSetName(alloc::string::String::from(id.name())))
            .collect();
        shell.set_cursors(cursor_sets, &mut compositor);
        // The font store is read once per boot, so its families are listed
        // once, before the stored text choice is resolved against them.
        shell
            .session_mut()
            .set_font_families(tairix_font::families());
        let mut clipboard =
            tairix_desktop_session::clipboard::SessionClipboard::new(RtPayloadRegions);

        // Every way out of this function stops every worker. The guard is
        // declared after the handles, so it runs first: the desks stop, then the
        // handles detach.
        let _worker_guard = WorkerGuard {
            listings: alloc::sync::Arc::clone(&listings),
            wallpapers: alloc::sync::Arc::clone(&wallpapers),
            artworks: alloc::sync::Arc::clone(&artworks),
            publisher: alloc::sync::Arc::clone(&publisher),
            catalogs: alloc::sync::Arc::clone(&catalogs),
        };

        // The desktop's icon artwork — the shipped `/System/Graphics` masters
        // and each bundle's own icon — is read through the session's own VFS
        // identity and decoded in a sandbox worker. With a decoder thread that
        // happens off this task and a paint that misses draws the built-in
        // glyph until the pixels land; without one the read and the round trip
        // happen here, exactly as they used to. Until this call the shell draws
        // every icon from its built-in glyphs, and it falls back to them again
        // whenever either seam refuses.
        if artwork_readers {
            shell.set_artwork_resolver(alloc::boxed::Box::new(DeferredArtwork(
                alloc::sync::Arc::clone(&artworks),
            )));
        } else if keys.is_some() {
            shell.set_artwork_resolver(alloc::boxed::Box::new(InlineArtwork::new(
                ArtworkFileReader(VfsFileReader),
                SandboxRasteriser {
                    sandbox: alloc::rc::Rc::clone(&sandbox),
                },
            )));
        }

        // The program library and what each installed application opens: the
        // machine store, the logged-in user's overlay, and one bundle manifest
        // per catalogued application, all read under the session's own
        // identity. A layer or manifest that cannot be used is reported loudly
        // and contributes nothing, so the desktop comes up with a calm empty
        // library rather than dying over a settings file.
        //
        // Read on this task, unlike every later refresh: no window is on
        // screen yet and nothing can be clicked, so there is no frame to owe
        // anyone — and the very first double-click on a document should find
        // the application that opens it rather than a not-yet-scanned
        // catalogue.
        let mut programs = Programs::new();
        adopt_programs(
            load_programs(&mut VfsFileReader, &mut RtHost, home_dir().as_deref()),
            &mut shell,
            &mut compositor,
            &mut programs,
        );

        // The icon bar's application strip: one slot per running
        // application, resolved from the bundle the kernel attested owns
        // each process. Nothing is loaded from disk — the strip is derived
        // from live state, never stored — so it starts empty and fills as
        // applications declare a presence or open a window.
        let mut apps = AppBarPanel::new();

        // The desktop's own icon column: the logged-in user's `Desktop`
        // folder, listed through the same capability-checked directory call
        // the trusted picker uses, under the session's own identity. An
        // unset or malformed `HOME` leaves the folder at the storage root's
        // `Desktop`, which simply lists nothing if it is not there.
        let mut desktop_folder = tairix_rt::env_var(b"HOME")
            .and_then(|home| core::str::from_utf8(home).ok())
            .and_then(|home| tairix_browse::vfs::components_from_absolute_path(home).ok())
            .unwrap_or_default();
        desktop_folder.push(alloc::string::String::from(
            tairix_abi::home::HOME_DESKTOP_DIR,
        ));
        let mut desktop = Desktop::new(
            AsyncDirectorySource {
                listings: alloc::sync::Arc::clone(&listings),
                client: ListingClient::Pinboard,
            },
            desktop_folder,
        )
        .acting_for(self_origin.uid());
        // The user's pinboard settings, with the same fail-closed posture as
        // the program library: absent → the defaults, silently (a fresh
        // account); unusable → the defaults plus one loud reason. Applied
        // *before* the first listing, so the very first frame already has
        // the user's own sort order and icon arrangement rather than
        // re-sorting a frame later.
        shell.set_zones(load_zones());
        // Armed before the zone is read, so a change between the two is not
        // missed.
        let machine_settings = watch_machine_settings(set);
        let _ = hold_machine_zone(read_machine_zone(&mut VfsFileReader), &mut shell);
        let zone_worker: alloc::sync::Arc<ZoneWorker> = alloc::sync::Arc::new(Worker::new(
            drain_machine_zone,
            machine_settings,
            tairix_rt::sync::WorkerWake::create(),
        ));
        if let Err(reason) = Worker::start(&zone_worker) {
            app::report(
                APP_NAME,
                format_args!(
                    "no time zone thread ({reason:?}); a change to the machine's zone is read on the serve loop"
                ),
            );
        }
        let _zone_guard = tairix_rt::work::WorkerGuard::new(&zone_worker);
        let mut pinboard = load_pinboard(
            &mut desktop,
            &mut shell,
            &mut compositor,
            &publisher,
            sandbox,
        );
        report_passed_over(shell.adopt_civil(desktop.settings()), desktop.settings());
        // The resolution the user chose is drawn from the first frame, where
        // this output offers it; nothing is on the screen yet to re-lay.
        let output = (mode.width_px, mode.height_px);
        let size = desktop.settings().resolution.on(output);
        if size != output {
            if compositor.set_desktop_size(size.0, size.1) {
                shell.set_output_layout(bar_config(compositor.screen_rect()), &mut compositor);
                // An empty screen keeps the one in force; the seat follows the
                // device either way before the first frame.
                let _ = pointer.set_screen(compositor.screen_rect(), compositor.scale());
            } else {
                report_undrawable(size);
            }
        }
        // What the live size was last brought to, so the loop re-lays the
        // screen only when the resolution or the output it is drawn on moved.
        let mut sized_for: Option<(Resolution, (u32, u32))> =
            Some((desktop.settings().resolution, output));
        // Where the windows sat around the open trial's resize, and those of a
        // reverted trial, put back once its convergence has redrawn the size.
        let mut trial_placements: Option<TrialPlacements> = None;
        let mut owed_placements: Option<TrialPlacements> = None;
        desktop.relist();
        // The wallpaper the desktop layer is painted over: read under the
        // session's own identity and fitted to this screen in the sandbox
        // worker, once. A wallpaper that cannot be read or rendered leaves
        // the backdrop colour showing and states why — the desktop never
        // fails over a picture.
        let backdrop_ready = prepare_wallpaper(
            &mut pinboard,
            &wallpapers,
            &mut shell,
            &desktop,
            &compositor,
            tairix_rt::clock_get(),
        );

        // The session-side served-window table. Declared before the first
        // present because every present reports which served windows that
        // frame has just put on screen, and bring-up presents before any
        // application can have opened one.
        let mut windows = SessionWindows::new();
        // The seat's one menu chain. Every menu on the desktop — an
        // application's and the desktop's own — is this one service's.
        let mut menu = MenuChain::new();
        // The seat's drain, and the answer sink the window-server path hands
        // the chain's delivery point. A chain displaced there is dismissed,
        // never chosen, so no bar answer lands in it; chosen rows are the
        // seat drain's, which routes them in the wake they were chosen in.
        let mut seat_drain = SeatDrain::new();
        let mut answered: Vec<tairix_desktop_session::ShellOutcome> = Vec::new();
        // First frame: place the bar, paint the desktop's icons beneath
        // every window, install the pointer cursor at the seat's initial
        // pointer position, and push the whole surface once;
        // every later present carries only the composited damage. The cursor
        // is then kept live by the shell as each seat event is pumped.
        // The device resolves its first motion from where it starts, so the
        // seat starts there too and the first frame draws the cursor there.
        shell.track_pointer(pointer.pointer(), &mut compositor);
        shell.present(&mut compositor);
        shell.present_desktop(&mut compositor, &desktop);
        shell.refresh_cursor(&mut compositor);
        // The login screen faded to black before it exited, so the desktop
        // comes up over a dark screen and reveals itself rather than
        // snapping on. Begun here, with the first frame composed and about
        // to be shown: begun any earlier, the fade would spend itself on
        // bring-up with nothing on screen yet.
        let mut fade = ScreenFade::begin(tairix_rt::clock_get(), &mut compositor);
        // The reveal witness says a user can see the desktop, so it waits for the
        // backdrop they chose rather than announcing a frame that carries the
        // fallback colour in its place — and, once that backdrop lands, for it to
        // finish dissolving in over that colour.
        fade.set_awaiting_backdrop(!backdrop_ready || !shell.backdrop_settled());
        // The trusted file picker (AW5/CU6): the one shared browser engine
        // over the session's own capability-checked listing call. Every
        // pick starts from a fresh listing under the session's authority;
        // the app never lists anything itself. The picker opens where a bare
        // file-manager window does, among the logged-in user's own files
        // (`HOME`, exported by login); an unset or malformed `HOME` opens it
        // at the root, and a start that cannot be listed climbs to the
        // nearest folder above it that can (fail closed, never a guessed
        // path).
        //
        // Built before the first present because every present announces the
        // picker it has newly carried, and one announcement path serves them
        // all.
        let picker_start = tairix_rt::env_var(b"HOME")
            .and_then(|home| core::str::from_utf8(home).ok())
            .and_then(|home| tairix_browse::vfs::components_from_absolute_path(home).ok())
            .map(|home| {
                let [files, _] = tairix_browse::bare_open_places(&home);
                files
            })
            .unwrap_or_default();
        let mut picker = SessionPicker::new(PickerListings(alloc::sync::Arc::clone(&listings)))
            .starting_at(picker_start);
        if let Err(code) = present(
            &mut shell,
            &mut compositor,
            &mut display,
            &mut fade,
            &mut windows,
            &mut menu,
            &mut picker,
            (&mut apps.service, &mut desktop),
            &mut suspended,
            TextDrawn {
                fonts: &font_worker,
                generation: tairix_font::text_generation(),
            },
        ) {
            return code;
        }

        // Bind the reserved window rendezvous. The kernel authorises the
        // bind by this session's kernel-attested live seat lease (the one
        // seat-scoped reserved id); the endpoint is unrestricted-sender —
        // the engine attests every caller per request and keys each window
        // to its creator, so an unentitled sender only ever reaches typed
        // refusals.
        let empty = CapabilitySet::empty();
        if tairix_rt::call_create(
            WINDOW_ENDPOINT,
            &empty,
            &empty,
            WINDOW_MAX_REQUEST,
            WINDOW_REPLY_MAX,
            WINDOW_CALLER_DEPTH,
        ) != 0
        {
            return app::fail(
                APP_NAME,
                EXIT_NO_WINDOW_ENDPOINT,
                "window endpoint bind refused",
            );
        }

        // Bind the notification rendezvous the same way: the same live seat
        // lease authorises it (it is the other seat-scoped reserved id), and
        // it is unrestricted-sender — a producer's identity is attested per
        // request and each notification keyed to it, so an unentitled sender
        // only ever reaches a typed refusal. A refusal here is the same
        // lease/rendezvous anomaly the window bind would hit; fail loud.
        if tairix_rt::call_create(
            NOTIFY_ENDPOINT,
            &empty,
            &empty,
            NOTIFY_MAX_REQUEST,
            STATUS_REPLY_LEN,
            NOTIFY_CALLER_DEPTH,
        ) != 0
        {
            return app::fail(
                APP_NAME,
                EXIT_NO_NOTIFY_ENDPOINT,
                "notification endpoint bind refused",
            );
        }

        // Bind the Switchboard rendezvous the same way: the third
        // seat-scoped reserved id, authorised by the same live seat lease,
        // unrestricted-sender — the serve arm attests the one legitimate
        // publisher (the Switchboard child this session spawns) per
        // request and refuses everyone else, so an unentitled sender only
        // ever reaches a typed refusal.
        if tairix_rt::call_create(
            SWITCHBOARD_ENDPOINT,
            &empty,
            &empty,
            SWITCHBOARD_MAX_REQUEST,
            SWITCHBOARD_PUBLISH_REPLY_LEN,
            SWITCHBOARD_DEPTH,
        ) != 0
        {
            return app::fail(
                APP_NAME,
                EXIT_NO_SWITCHBOARD_ENDPOINT,
                "switchboard endpoint bind refused",
            );
        }

        // Bind the pinboard rendezvous the same way: the fourth seat-scoped
        // reserved id, authorised by the same live seat lease,
        // unrestricted-sender — the serve arm compares every caller's
        // kernel-attested origin uid against this session's own and refuses
        // anything else, so an unentitled sender only ever reaches a typed
        // refusal.
        if tairix_rt::call_create(
            PINBOARD_ENDPOINT,
            &empty,
            &empty,
            PINBOARD_MAX_REQUEST,
            PINBOARD_MAX_REPLY,
            PINBOARD_CALLER_DEPTH,
        ) != 0
        {
            return app::fail(
                APP_NAME,
                EXIT_NO_PINBOARD_ENDPOINT,
                "pinboard endpoint bind refused",
            );
        }

        // Park on the wait-set: the seat member wakes on input delivery
        // and on lease loss, the endpoint member on a posted window
        // request, the any-child member when a spawned app exits (so its
        // windows are torn down promptly), and the memory-pressure member
        // when the machine's pressure band moves. Every member is
        // owner-checked at add; the session never polls and never sleeps
        // through its own revocation.
        if tairix_rt::waitset_ctl(
            set,
            WaitSetOp::Add,
            WaitSourceKind::SeatInput,
            SEAT_PRIMARY,
            SEAT_TOKEN,
        ) != 0
        {
            return app::fail(APP_NAME, EXIT_WAIT_FAILED, "seat wait refused");
        }
        if tairix_rt::waitset_ctl(
            set,
            WaitSetOp::Add,
            WaitSourceKind::Endpoint,
            WINDOW_ENDPOINT,
            WINDOW_TOKEN,
        ) != 0
        {
            return app::fail(APP_NAME, EXIT_WAIT_FAILED, "window endpoint wait refused");
        }
        if tairix_rt::waitset_ctl(
            set,
            WaitSetOp::Add,
            WaitSourceKind::Endpoint,
            NOTIFY_ENDPOINT,
            NOTIFY_TOKEN,
        ) != 0
        {
            return app::fail(
                APP_NAME,
                EXIT_WAIT_FAILED,
                "notification endpoint wait refused",
            );
        }
        if tairix_rt::waitset_ctl(
            set,
            WaitSetOp::Add,
            WaitSourceKind::Endpoint,
            SWITCHBOARD_ENDPOINT,
            SWITCHBOARD_TOKEN,
        ) != 0
        {
            return app::fail(
                APP_NAME,
                EXIT_WAIT_FAILED,
                "switchboard endpoint wait refused",
            );
        }
        if tairix_rt::waitset_ctl(
            set,
            WaitSetOp::Add,
            WaitSourceKind::Endpoint,
            PINBOARD_ENDPOINT,
            PINBOARD_TOKEN,
        ) != 0
        {
            return app::fail(APP_NAME, EXIT_WAIT_FAILED, "pinboard endpoint wait refused");
        }
        if tairix_rt::waitset_ctl(
            set,
            WaitSetOp::Add,
            WaitSourceKind::Child,
            tairix_abi::WAITSET_CHILD_ANY,
            CHILD_TOKEN,
        ) != 0
        {
            return app::fail(APP_NAME, EXIT_WAIT_FAILED, "child wait refused");
        }
        // The workers' wake: readable exactly when a directory read or a
        // wallpaper preparation has finished. A refused add is fatal rather than
        // tolerated — a worker whose answers nobody collects would leave the
        // desktop listing forever, and the session must not park on a set that
        // cannot report it.
        if let Some(read) = worker_wake.read_end() {
            if tairix_rt::waitset_ctl(
                set,
                WaitSetOp::Add,
                WaitSourceKind::Stream,
                u64::from(read),
                WORKER_TOKEN,
            ) != 0
            {
                return app::fail(APP_NAME, EXIT_WAIT_FAILED, "listing wake wait refused");
            }
        }
        // `watch` re-reads the band as it registers the member, closing the
        // race between the bring-up read above and this registration — a
        // move in between would otherwise never be seen.
        if !tairix_procinfo::pressure::watch(set, PRESSURE_TOKEN) {
            return app::fail(APP_NAME, EXIT_WAIT_FAILED, "memory-pressure wait refused");
        }
        if let Some(read) = sound_worker.wake().read_end() {
            if tairix_rt::waitset_ctl(
                set,
                WaitSetOp::Add,
                WaitSourceKind::Stream,
                u64::from(read),
                SOUND_TOKEN,
            ) != 0
            {
                return app::fail(APP_NAME, EXIT_WAIT_FAILED, "sound wake wait refused");
            }
        }
        if let Some(read) = private_worker.wake().read_end() {
            if tairix_rt::waitset_ctl(
                set,
                WaitSetOp::Add,
                WaitSourceKind::Stream,
                u64::from(read),
                PRIVATE_TOKEN,
            ) != 0
            {
                return app::fail(
                    APP_NAME,
                    EXIT_WAIT_FAILED,
                    "sound-choices wake wait refused",
                );
            }
        }
        if let Some(read) = unlock_worker.wake().read_end() {
            if tairix_rt::waitset_ctl(
                set,
                WaitSetOp::Add,
                WaitSourceKind::Stream,
                u64::from(read),
                UNLOCK_TOKEN,
            ) != 0
            {
                return app::fail(APP_NAME, EXIT_WAIT_FAILED, "unlock wake wait refused");
            }
        }
        if let Some(read) = elevate_worker.wake().read_end() {
            if tairix_rt::waitset_ctl(
                set,
                WaitSetOp::Add,
                WaitSourceKind::Stream,
                u64::from(read),
                ELEVATE_TOKEN,
            ) != 0
            {
                return app::fail(APP_NAME, EXIT_WAIT_FAILED, "elevation wake wait refused");
            }
        }
        if let Some(read) = zone_worker.wake().read_end() {
            if tairix_rt::waitset_ctl(
                set,
                WaitSetOp::Add,
                WaitSourceKind::Stream,
                u64::from(read),
                MACHINE_ZONE_TOKEN,
            ) != 0
            {
                return app::fail(APP_NAME, EXIT_WAIT_FAILED, "time zone wake wait refused");
            }
        }
        if let Some(read) = font_worker.wake().read_end() {
            if tairix_rt::waitset_ctl(
                set,
                WaitSetOp::Add,
                WaitSourceKind::Stream,
                u64::from(read),
                FONT_TOKEN,
            ) != 0
            {
                return app::fail(APP_NAME, EXIT_WAIT_FAILED, "font wake wait refused");
            }
        }
        for (read, token, what) in [
            (
                saver_fonts.wake().read_end(),
                SAVER_FONT_TOKEN,
                "saver font wake wait refused",
            ),
            (
                saver_files.wake().read_end(),
                SAVER_FILE_TOKEN,
                "saver file wake wait refused",
            ),
            (
                capture_files.wake().read_end(),
                CAPTURE_FILE_TOKEN,
                "capture file wake wait refused",
            ),
            (
                held_writes.wake().read_end(),
                HELD_WRITES_TOKEN,
                "held-writes wake wait refused",
            ),
        ] {
            if let Some(read) = read {
                if tairix_rt::waitset_ctl(
                    set,
                    WaitSetOp::Add,
                    WaitSourceKind::Stream,
                    u64::from(read),
                    token,
                ) != 0
                {
                    return app::fail(APP_NAME, EXIT_WAIT_FAILED, what);
                }
            }
        }
        // A member reports a generation that moves after it joins, so each
        // is read once after joining and nothing published in between is
        // missed.
        for (topic, token) in [
            (NoticeTopic::AudioDevices, AUDIO_DEVICES_TOKEN),
            (NoticeTopic::AudioCapture, AUDIO_CAPTURE_TOKEN),
            (NoticeTopic::CueAuthority, CUE_AUTHORITY_TOKEN),
        ] {
            if tairix_rt::waitset_ctl(
                set,
                WaitSetOp::Add,
                WaitSourceKind::SystemNotice,
                u64::from(topic.as_u32()),
                token,
            ) != 0
            {
                return app::fail(APP_NAME, EXIT_WAIT_FAILED, "sound notice wait refused");
            }
        }
        if tairix_rt::waitset_ctl(
            set,
            WaitSetOp::Add,
            WaitSourceKind::SystemNotice,
            u64::from(NoticeTopic::Mounts.as_u32()),
            MOUNTS_TOKEN,
        ) != 0
        {
            return app::fail(
                APP_NAME,
                EXIT_WAIT_FAILED,
                "mount-table notice wait refused",
            );
        }
        // The table the notice moves on from is read once, as it is joined.
        desktop.mounts_moved();
        ask_for_mounts(&listings, &mut desktop, &mut shell, &mut compositor);
        serve_held(
            (&held_writes, &mut held_questions),
            true,
            &mut windows,
            &mut shell,
            &mut compositor,
        );
        shell.set_sound(&mut compositor, sound.captures(live_captures()));
        effects.authority_is(cue_authority());
        pump_private(
            &mut effects,
            &mut memory,
            &private_worker,
            tairix_rt::clock_get(),
        );
        sound.refresh();
        pump_sound(
            &mut sound,
            &sound_worker,
            &mut cue_posts,
            set,
            &publisher,
            &mut pinboard,
            &wallpapers,
            &mut desktop,
            &mut shell,
            &mut compositor,
        );
        // The wake mailbox joins for the session's whole life, foreground or
        // background: it is the only member a switched-away desktop waits
        // on, so a bind that succeeded and a member that did not join would
        // park it forever. A session that never bound has no member, and
        // offers no switch.
        if let Some(wake) = switch.wake_endpoint() {
            if tairix_rt::waitset_ctl(set, WaitSetOp::Add, WaitSourceKind::Port, wake, WAKE_TOKEN)
                != 0
            {
                return app::fail(
                    APP_NAME,
                    EXIT_WAIT_FAILED,
                    "session wake mailbox wait refused",
                );
            }
        }

        // The window channel's server state: the engine, the kernel-attested
        // caller identity, the app-ward event sink, and the focused served
        // window the routing mirrors (the window table itself is older than
        // this block — every present announces what it put on screen, so it
        // exists before the first frame). The engine stamps this session's
        // own kernel-attested identity into every create reply, so apps can
        // authenticate the sender of each later event.
        // What one client may hold mapped here, from the machine's own RAM
        // total and one frame of this session's output — never a window count,
        // which says nothing about the bytes a window actually costs.
        let mut server = WindowServer::new(
            RtShmMapper,
            self_origin.proc_id(),
            tairix_window::client_frame_budget_bytes(memory_total, frame_len),
        );
        let mut identity = RtWindowIdentity::new();
        let mut sink = RtEventSink::new(set);
        let mut focused: Option<u64> = None;
        // Every app launched from the desktop is admitted immediately and
        // loads on its own task (asynchronous launch); a load refusal now
        // surfaces as the child's reserved-`LOAD_*` exit status, not the
        // `spawn` return. This table remembers each running child's label
        // (so the `CHILD_TOKEN` reap below can name the app in the
        // fail-loud diagnosis) and its spawn path (the attested bundle
        // identity a single-instance rule resolves against). An entry is
        // removed when its child is reaped, so it never grows beyond the
        // apps currently alive.
        let mut launched = LaunchTable::new();
        // Start the desktop's Switchboard monitor. It is recorded in the
        // launch table like any desktop child — the reap arm names a load
        // refusal, and the serve arm attests its calls against this entry
        // — and the very same bring-up serves a later tray press that
        // finds no instance live.
        // Spawned outright rather than through the launch funnel, for the
        // same reason the file manager below is: the table is empty here, so
        // there is no instance for the funnel to find.
        let mut switchboard_pid =
            spawn_and_record(&mut launched, SWITCHBOARD_RUN_PATH, SWITCHBOARD_LABEL, &[]);
        // Which window owners the live monitor has been told the bundle of, so
        // a launch costs one send and a fresh instance is told everything.
        let mut owner_bundles = OwnerBundleGate::new();
        // Whether the live monitor publishes the machine reports a System
        // Monitor screensaver draws.
        let mut machine_watch = MachineWatch::new();
        // Start the desktop's file manager. It is a component of the desktop,
        // not an application the user starts, so it comes up with the session
        // and holds its icon-bar slot from here on.
        spawn_files(&mut launched);
        let mut file_manager = FileManagerKeeper::default();
        // A tray press with no live monitor to receive it: the section the
        // bar asked to open on, held until that instance's first publish
        // proves it is up. One pending open, replaced by a later press
        // rather than queued, so a user pressing repeatedly opens the
        // section they last asked for and no more.
        let mut pending_open: Option<CommandSection> = None;
        // What the monitor's Resources page already shows about the last
        // frame, so a frame whose cost is unchanged sends nothing — and, past
        // that, so a desktop whose cost moves on *every* frame (a pointer
        // crossing the wallpaper redamages the cursor) still reports at a
        // rate a reader can follow rather than at frame rate.
        let mut frames = FrameReportGate::new();
        let mut frame_stats = FrameStatsPublisher::new();
        // The channel that accounting goes out over, held for the life of the
        // loop because it carries the submission in flight.
        let mut frame_sink = RtFrameStatsSink::new();
        // The frame deadline. Wakes arrive as fast as a hand can move a
        // mouse, which is several times faster than any screen shows a
        // frame, so damage accumulates in the compositor between deadlines
        // and is composited once when one arrives. A held frame shortens the
        // park below to the moment it comes due and nothing else; a desktop
        // with nothing held arms nothing.
        let mut pacer = FramePacer::new();
        // The trusted confirmation prompt for a power transition. It is the
        // session's own window, so the question the user answers is asked by
        // the desktop itself rather than by the bar, which holds no
        // authority; an unanswered prompt relays nothing.
        let mut confirm = ConfirmPrompt::new();
        // The system authorisation prompt, for a program that needs authority
        // the signed-in account may not hold. The session draws it and holds
        // the seat for it, so a password is typed into desktop chrome and
        // offered to the console's broker, never to an application; an
        // unanswered prompt offers nothing.
        let mut elevate = AuthorisePrompt::new();
        // A new desktop resolution on trial: drawn at once, put to the user,
        // and kept only if they keep it.
        let mut resolution = ResolutionPrompt::new();
        // The screen lock, and the account it re-verifies. `USER` is what
        // login exported for this session; it names whose password the
        // prompt is asking for and nothing more — the broker reads the
        // identity it actually checks against from the kernel, so a wrong
        // or missing name here cannot unlock anybody's session. An unset or
        // malformed value simply leaves the prompt unnamed.
        let mut lock = ScreenLock::new();
        // The idle deadline the screensaver and the idle lock ride, and the
        // screensaver it puts up.
        let mut idle = IdleClock::new(tairix_rt::clock_get());
        let mut idle_policy = IdlePolicy::default();
        let mut saver: DesktopSaver = Screensaver::new(tairix_rt::LogSink);
        // The pipes of the saver drawing, watched in the directions it wants.
        let mut saver_members = SessionMembers::new(set, SAVER_READ_TOKEN, SAVER_WRITE_TOKEN);
        let child_reaper = alloc::sync::Arc::new(ChildReaper::new(set));
        // The screen capture and the recording it makes. What an earlier
        // session left mid-capture is named first.
        let mut capture = CaptureController::new(desktop.settings().capture);
        let mut recorder = Recorder::new(set);
        let capture_producer = tairix_desktop_session::producer_of(&self_origin).ok();
        let swept = capture_files.submit(CaptureJob::Sweep {
            civil: shell.clock().civil(),
        });
        if swept.is_err() {
            app::report(
                APP_NAME,
                format_args!("captures cut short are named at the next start"),
            );
        }
        // The savers the desktop offers: its own two until the stores are read,
        // which is asked for at once, and again whenever Settings lists them
        // from the start.
        let mut savers = SaverCatalog::default();
        let mut scan_owed = false;
        if scan_savers(&saver_files, &mut scan_owed) {
            collect_saver_files(&saver_files, &mut saver, &mut savers, &mut compositor);
        }
        // The places of the shipped pictures the drawing saver's slides feed
        // offers it.
        let mut saver_slides: alloc::vec::Vec<usize> = alloc::vec::Vec::new();
        // The cores a saver may run a thread on, and the memory a saver's
        // ceiling is a share of.
        let saver_machine = (u16::try_from(online).unwrap_or(u16::MAX), memory_total);
        let account = tairix_rt::env_var(b"USER")
            .and_then(|raw| core::str::from_utf8(raw).ok())
            .unwrap_or_default();
        let mut elevator = RtElevator {
            worker: alloc::sync::Arc::clone(&elevate_worker),
            tickets: Tickets::default(),
            own: alloc::string::String::from(account),
            unasked: None,
        };
        // What this account is *called*, as the login screen showed it: a mark
        // derived from the login name instead differs from that screen's.
        // Unset leaves the surfaces unnamed rather than guessing.
        let shown_name = tairix_rt::env_var(ENV_SHOWN_NAME.as_bytes())
            .and_then(|raw| core::str::from_utf8(raw).ok())
            .unwrap_or_default();
        // The bar's trailing capsule wears this account's identity disc, the
        // same mark the login screen drew for it.
        shell.set_account(&mut compositor, shown_name);
        // What the machine is, read here because it cannot be read on the
        // loop: a screensaver is told the machine's name, and *About This
        // System* states its version.
        let machine = tairix_procinfo::system_identity(&IpcTransport).ok();
        let host_name = machine.as_ref().map_or_else(
            || {
                io::write_stderr_line(
                    "desktop: the machine's name could not be read; a screensaver is told only the account",
                );
                alloc::string::String::new()
            },
            |machine| tairix_procinfo::field_lossy(machine.hostname_bytes()),
        );
        let system = SystemFacts {
            version: machine
                .as_ref()
                .map(tairix_abi::sysinfo::SystemIdentity::version),
            processor: cpus.as_deref().and_then(tairix_procinfo::processor_summary),
            memory_bytes: memory_read,
        };
        // A display with no power control of its own is said so once a
        // session, not at every idle, and one that will not light again once
        // until it does, not at every input.
        let mut told_unswitchable = false;
        let mut told_unwakeable = false;
        // What tells this desktop's own Settings application apart from a
        // bundle merely claiming its identifier.
        shell.set_own_app(self_origin.app().copied());
        // Offer the rows that need re-authentication only where this session
        // really has a broker for it: the Lock row (which would otherwise
        // strand the user behind a screen with no way back) and the clock's
        // set-time row (which needs an account holding CAP_TIME_SET
        // authenticated). One console fact, read once.
        shell.set_elevation_available(
            &mut compositor,
            elevate_endpoint(self_origin.console()).is_ok(),
        );
        // Offer the Switch User row only where this session really could be
        // resumed: the wake mailbox bound. Without it the row is absent
        // rather than refused — there is no authority to explain.
        shell.set_switch_user_available(&mut compositor, switch.is_available());
        // The clock's first reading, so the bar carries the time from the
        // frame the user first sees rather than from the next minute.
        tick_clock(&mut shell, &mut compositor, tairix_rt::clock_get());

        let mut token = 0u64;
        // A member whose report the wait consumed on a turn an idle deadline
        // took, served on the next turn without waiting.
        let mut owed_wake = None;
        // Held for the life of the loop rather than taken per request: it is
        // sized to the widest operation the channel has, so a per-request
        // array would cost every present — the hottest and one of the
        // shortest — the whole of the widest one's clearing.
        let mut request = [0u8; WINDOW_MAX_REQUEST];
        // Held for the same reason, and it matters more: the widest reply
        // carries a path, so a per-request array would put four kibibytes of
        // clearing on every present.
        let mut reply = [0u8; WINDOW_REPLY_MAX];
        // The look the retained prompts were last painted in.
        let mut prompts_style = shell.style_generation();
        // What deferred font fetches had brought when the desktop last laid
        // its text out.
        let mut text_drawn = tairix_font::text_generation();
        // Whether the screen was locked or handed to another session as of the
        // last turn, so a key held across that edge stops repeating.
        let mut was_screened = false;
        // Set once the session is ending, until its applications have closed.
        let mut departure: Option<Departure> = None;
        loop {
            // Each listed bundle's version is asked of the readers once a turn.
            shell.begin_artwork_turn();
            if let Some(leaving) = departure.as_mut() {
                let unasked = leaving.unasked(windows.top_level());
                for window_id in unasked {
                    deliver(
                        &mut server,
                        &mut sink,
                        &mut shell,
                        &mut compositor,
                        &mut windows,
                        &mut picker,
                        &mut apps.service,
                        &mut menu,
                        &WindowEvent::CloseRequested { window_id },
                    );
                }
                if leaving.is_complete(
                    tairix_rt::clock_get(),
                    windows.top_level().next().is_some(),
                    recorder.recording.is_some(),
                ) {
                    fade_to_black(&mut fade, &mut compositor, &mut display);
                    shell.teardown(&mut compositor);
                    see_out_private(&mut memory, &private_worker);
                    return leaving.exit_code();
                }
            }
            // However a window closed, what it asked of the preview desk and
            // of the authorisation prompt goes with it before the next park.
            for window_id in windows.take_closed() {
                wallpapers.forget_window(window_id);
                elevate.abort_for(AuthoriseFor::Window(window_id), &mut compositor);
            }
            // So does what an application that has gone asked from its icon-bar
            // slot.
            for owner in windows.take_withdrawn() {
                elevate.abort_for(AuthoriseFor::Slot(owner), &mut compositor);
            }
            // So does each step a drop's source took, and each drop a window's
            // going ended: the desktop stands what they made, and its artwork
            // holds back what is being written.
            adopt_landings(
                windows.take_landings(),
                &mut desktop,
                &artworks,
                (&mut shell, &mut compositor),
            );
            // However a prompt ended, whoever asked is told before the next
            // park, and what the desktop owes an administrator is asked through
            // the one prompt.
            if let Some(task) = desktop.take_administrator_ask() {
                let busy = elevate.is_up();
                let asked = !busy
                    && !lock.is_locked()
                    && tairix_desktop_session::systask::request_for(&task, AuthoriseAsker::Desktop)
                        .is_ok_and(|request| {
                            elevate.ask(request, &mut elevator, &shell, &mut compositor)
                        });
                if !asked {
                    desktop.administrator_unasked(busy);
                }
            }
            for concluded in elevate.take_concluded() {
                let asker = match concluded.asker {
                    AuthoriseAsker::App(asker) => asker,
                    AuthoriseAsker::Desktop => {
                        desktop.administrator_answered(concluded.outcome);
                        continue;
                    }
                    AuthoriseAsker::Recording => {
                        recorder.administrator_answered(concluded.wired);
                        continue;
                    }
                };
                settle_authorisation(
                    (asker, concluded.outcome, concluded.output),
                    &mut server,
                    &mut sink,
                    &mut shell,
                    &mut compositor,
                    &mut windows,
                    &mut picker,
                    &mut apps,
                    &mut menu,
                );
            }
            // Whatever path adopted a settings change, the seat's sources, the
            // pointer aids and the window manager are brought to it here,
            // before the next park.
            let aids_now = AidPolicy::of(desktop.settings());
            if aids_now != shell.pointer_aids() {
                shell.set_pointer_aids(aids_now);
            }
            let input_now = InputPolicy::of(desktop.settings());
            if input_now != input_policy {
                drive_pointer(&mut pointer, &input_now);
                keyboard.set_repeat(input_now.repeat);
                compositor.set_double_click(input_now.double_click);
                if input_now.double_click != input_policy.double_click {
                    publish_desktop(&compositor, &shell);
                }
                input_policy = input_now;
            }
            // A key held into a lock, or into another user's session, must not
            // go on repeating there; one pressed at the lock repeats as usual.
            let screened = lock.is_locked() || switch.is_background();
            if screened && !was_screened {
                keyboard.cancel_repeat();
            }
            was_screened = screened;
            let idle_now = IdlePolicy::of(desktop.settings(), shell.can_lock());
            if idle_now != idle_policy {
                idle.set_policy(idle_now);
                idle_policy = idle_now;
            }
            // A screen handed to another session is theirs to blank, and the
            // display service lights it for them.
            if switch.is_background() && saver.dismiss(&mut compositor, None) == Ok(true) {
                wallpapers.forget_slides();
            }
            // What was opened while the file manager was starting is handed to
            // it once it can take it.
            if launched.holds_for_file_manager() {
                LaunchCtx {
                    launched: &mut launched,
                    apps: &apps.service,
                    server: &mut server,
                    sink: &mut sink,
                    windows: &windows,
                    identity: &identity,
                }
                .deliver_held_for_file_manager((&mut shell, &mut compositor));
            }
            // A System Monitor coming up is the demand for the monitor that
            // feeds it, so one found not running is started once — at the
            // board's start, never in a loop over a monitor that keeps dying —
            // and the board says so when it will not start.
            if machine_watch.want(saver.wants_machine_reports(), &mut RtSwitchboardMailbox)
                && switchboard_pid.is_none()
            {
                switchboard_pid = spawn_switchboard(
                    &mut LaunchCtx {
                        launched: &mut launched,
                        apps: &apps.service,
                        server: &mut server,
                        sink: &mut sink,
                        windows: &windows,
                        identity: &identity,
                    },
                    &mut shell,
                    &mut compositor,
                );
                if switchboard_pid.is_none() {
                    if let Some(end) =
                        saver.machine_unmonitored(tairix_rt::clock_get(), &mut compositor)
                    {
                        report_saver_end(&end);
                    }
                }
            }
            // A trial's countdown moves on whatever wake finds it due, and one
            // that has ended is put into effect here, where the settings and
            // the screen are both at hand.
            let _ = resolution.tick(tairix_rt::clock_get(), &mut shell, &mut compositor);
            if let Some(end) = resolution.take_ended() {
                let placements = trial_placements.take();
                if matches!(end, TrialEnd::Reverted(_)) {
                    owed_placements = placements;
                }
                conclude_trial(
                    end,
                    &publisher,
                    &mut pinboard,
                    &wallpapers,
                    &mut desktop,
                    &mut shell,
                    &mut compositor,
                );
                sized_for = None;
            }
            if let Some(answered) = publisher.take_kept_answered() {
                sized_for = None;
                let outcome = match answered {
                    Ok(()) => TrialOutcome::Kept,
                    Err(_) => TrialOutcome::Refused,
                };
                log_info(TRIAL_CONCLUDED, TRIAL_CONCLUDED_MESSAGE, &outcome.fields());
            }
            // Outside a trial, the desktop is drawn at the resolution the
            // settings in force name, wherever this output offers it. A lock
            // or a screensaver covers the size it was drawn at, so the desktop
            // waits for both to go.
            if !resolution.is_open()
                && !publisher.publishing()
                && !switch.is_background()
                && !lock.is_locked()
                && !saver.is_shown()
            {
                let output = compositor.mode();
                let wanted = (
                    desktop.settings().resolution,
                    (output.width_px, output.height_px),
                );
                if sized_for != Some(wanted) {
                    sized_for = Some(wanted);
                    let resized = resize_desktop(
                        wanted.0.on(wanted.1),
                        &mut Resize {
                            compositor: &mut compositor,
                            shell: &mut shell,
                            desktop: &mut desktop,
                            pinboard: &mut pinboard,
                            wallpapers: &wallpapers,
                            windows: &mut windows,
                            menu: &mut menu,
                            pointer: &mut pointer,
                            prompts: (&mut confirm, &mut elevate, &mut resolution),
                            saver: &mut saver,
                            lock: &mut lock,
                            idle: &mut idle,
                            memory_bytes,
                        },
                    );
                    if resized == Resized::Changed {
                        if let Some(placed) = owed_placements.take() {
                            let work_area = shell.work_area(&compositor);
                            windows.restore_placements(
                                &mut compositor,
                                (&placed.before, &placed.after),
                                work_area,
                            );
                        }
                        deliver_owed_events(
                            &mut server,
                            &mut sink,
                            &mut shell,
                            &mut compositor,
                            &mut windows,
                            &mut picker,
                            &mut apps.service,
                            &mut menu,
                        );
                    }
                }
            }
            // The park stays indefinite: a cache-report change the runtime's
            // rate limiter is holding back, a frame report this session's own
            // one is holding back, a composited frame the pacer is holding
            // for its deadline, an animation frame the session owes, a bar
            // gesture the clock owes an answer to, and a window thumbnail a
            // hover picker is waiting on, only ever *tighten* the wait to the
            // moment the work is due, and fold back to indefinite once it is
            // done. The desktop never polls for anything.
            //
            // A background session has no deadline at all, not even those:
            // it draws nothing, so a held-back report has nothing to report,
            // no held frame can reach a screen it does not own, nothing it
            // animates is on screen, and a timer would wake a core for no
            // work — but for the present answers its applications wait on.
            // Applied in turn rather than nested: nine levels of nesting said
            // nothing the order does not.
            if let Some(own) = picker.take_own() {
                conclude_own_pick(own, &mut effects, &mut memory, &private_worker);
            }
            if windows.alerts.held_answered() {
                serve_held(
                    (&held_writes, &mut held_questions),
                    false,
                    &mut windows,
                    &mut shell,
                    &mut compositor,
                );
            }
            // Input only records what it changed in an alert or prompt; the
            // drained run is painted here, once.
            windows.alerts.paint_owed(&shell, &mut compositor);
            confirm.paint_owed(&shell, &mut compositor);
            resolution.paint_owed(&shell, &mut compositor);
            picker.paint_owed(&mut shell, &mut compositor);
            post_push(&mut effects, &mut cue_posts, set);
            watch_saver(&mut saver_members, &mut saver, &mut compositor);
            serve_capture(
                (&mut capture, &mut recorder),
                (&mut shell, &mut compositor),
                &mut CaptureTurn {
                    session: self_origin.proc_id(),
                    screened: Screened {
                        lock: lock.window(),
                        sheet: elevate.wm_id(),
                    },
                    away: lock.is_locked() || departure.is_some() || switch.is_background(),
                    saver: saver.is_shown(),
                    files: &capture_files,
                    reaper: &child_reaper,
                    clipboard: &mut clipboard,
                    producer: capture_producer.as_ref(),
                    elevate: &mut elevate,
                    elevator: &mut elevator,
                },
            );
            capture.adopt(&desktop.settings().capture, publisher.publishing());
            if let Some(options) = capture.take_settled() {
                let mut settings = publisher.base(desktop.settings());
                settings.capture = options;
                if request_pinboard_settings(
                    settings,
                    &publisher,
                    None,
                    &mut pinboard,
                    &wallpapers,
                    &mut desktop,
                    &mut shell,
                    &mut compositor,
                    tairix_rt::clock_get(),
                ) {
                    shell.present_desktop(&mut compositor, &desktop);
                }
            }
            recorder.watch();
            let timeout_ns = {
                let now_ns = tairix_rt::clock_get();
                let refresh = compositor.refresh_rate();
                // With no frame reaching the screen no latch answers the
                // presents held behind one, so the pacer does, at the pace
                // the screen would have set.
                let dark = display.is_none() || !presentable(&saver, &lock, &compositor);
                if dark && server.holds_answers() && pacer.answers_due(now_ns, refresh) {
                    server.release_answers();
                }
                answer_presents(&mut server);
                let dark_held = dark && server.holds_answers();
                // A thumbnail slice is owed *now*: the wait still reports a
                // ready member first, so slicing never starves input.
                let owed = if shell.window_thumbnails_owed() {
                    0
                } else {
                    u64::MAX
                };
                let mut park = tairix_rt::cachereport::fold_wait_deadline_ns(owed);
                if presentable(&saver, &lock, &compositor) {
                    park = pacer.park_deadline_ns(now_ns, park, refresh);
                }
                park = frame_stats.park_deadline_ns(now_ns, park);
                park = frames.park_deadline_ns(now_ns, park);
                park = shell.taskbar_park_deadline_ns(now_ns, park);
                // A presented picker's arm can fall due with nothing to paint.
                picker.report_newly_shown(now_ns, false, || {
                    log_info(PICKER_SHOWN, PICKER_SHOWN_MESSAGE, &[]);
                });
                let held = picker
                    .scroll_repeat_deadline(now_ns)
                    .into_iter()
                    .chain(windows.alerts.repeat_deadline(now_ns))
                    .chain(picker.shown_deadline())
                    .min();
                park = park_within(park, held.map(|due| due.saturating_sub(now_ns)));
                park = shell.tooltip_park_deadline_ns(now_ns, park);
                park = menu.park_deadline_ns(now_ns, park);
                park = resolution.park_deadline_ns(now_ns, park);
                park = keyboard.park_deadline_ns(now_ns, park);
                park = pointer.park_deadline_ns(now_ns, park);
                park = idle.park_deadline_ns(now_ns, park);
                park = effects.park_deadline_ns(now_ns, park);
                park = memory.park_deadline_ns(now_ns, park);
                park = park_within(park, sink.retry_in(now_ns));
                // Exactly what `animate` steps, which it steps only on a
                // frame the pacer has due.
                let mut step = WAITSET_TIMEOUT_NONE;
                step = shell.backdrop_park_deadline_ns(now_ns, step, refresh);
                step = shell.library_park_deadline_ns(now_ns, step, refresh);
                step = shell.pointer_aids_park_deadline_ns(now_ns, step, refresh);
                step = shell.drag_park_deadline_ns(now_ns, step, refresh);
                step = fade.park_deadline_ns(now_ns, step, refresh);
                step = park_within(step, compositor.next_elevation_frame_in(now_ns));
                step = shell.clock().park_deadline_ns(now_ns, step);
                step = lock.park_deadline_ns(now_ns, step, refresh);
                step = elevate.park_deadline_ns(now_ns, step, refresh);
                park = pacer.step_deadline_ns(now_ns, park, step, refresh);
                park = saver.park_deadline_ns(now_ns, park);
                park = capture.park_deadline_ns(now_ns, park);
                let recording_park = recorder
                    .recording
                    .as_ref()
                    .map_or(WAITSET_TIMEOUT_NONE, |recording| {
                        recording.park_deadline_ns(now_ns, WAITSET_TIMEOUT_NONE)
                    });
                let park =
                    serve_park_ns(&switch, departure.as_ref(), now_ns, (park, recording_park));
                pacer.answers_deadline_ns(now_ns, park, refresh, dark_held)
            };
            // What the kernel had no memory for is offered again once its pause
            // is over, through the hold-back's own wake, however busy the loop.
            if owed_wake.is_none() && sink.retry_due() {
                owed_wake = Some(HOLDBACK_TOKEN);
            }
            let waited = if let Some(owed) = owed_wake.take() {
                token = owed;
                0
            } else {
                tairix_rt::waitset_wait(set, timeout_ns, &mut token)
            };
            // A held key's repeat, and a touch acting at its deadline, are
            // seat input the device never sent, so a wait that ended for one
            // is served exactly as the seat's input is.
            let repeat_due = waited != 0 && Errno::from_syscall(waited) == Errno::TimedOut && {
                let now_ns = tairix_rt::clock_get();
                keyboard.repeat_due(now_ns) || pointer.touch_due(now_ns)
            };
            if repeat_due {
                token = SEAT_TOKEN;
            } else if waited != 0 || idle.is_due(tairix_rt::clock_get()) {
                // An idle deadline is served on whatever wake finds it passed,
                // so a client that keeps the loop busy cannot hold the lock
                // off. The member that woke re-reports on the next wait, or,
                // where the wait consumed its report, is served next without
                // one.
                if waited != 0 && Errno::from_syscall(waited) != Errno::TimedOut {
                    // A dead wait-set would degrade the loop into a busy poll;
                    // exit fail-loud instead and let the supervisor decide.
                    return app::fail(APP_NAME, EXIT_WAIT_FAILED, "seat wait failed");
                }
                if waited == 0 && reports_once(token) {
                    owed_wake = Some(token);
                }
                // Nothing is dispatched on `token` here: after a timeout it
                // still names the *previous* wake's source, and dispatching
                // on it would block in a `call_recv` with nothing to receive.
                // Only what this loop armed the deadline for is owed: the next
                // frame of whatever is animating, the bar gesture the clock
                // owes an answer to, the next window thumbnail a hover picker
                // is waiting on, and the held-back report.
                //
                // One clock reading serves the whole frame: what the
                // animation steps to and what the report's rate limit is
                // timed against are the same instant, and the frame path
                // takes one syscall for both rather than two.
                let now_ns = tairix_rt::clock_get();
                // The pointer resting still produces no events at all, so
                // this is what opens a picker whose dwell has elapsed and
                // takes down one whose grace has, and steps a scrollbar press
                // the pointer holds still.
                shell.tick_taskbar(&mut compositor, now_ns);
                picker.scroll_repeat(now_ns, &mut shell, &mut compositor);
                windows.alerts.repeat(now_ns, &shell, &compositor);
                // Idleness produces no event, so this is where it is acted on.
                while let Some(action) = idle.due(now_ns) {
                    match action {
                        IdleAction::Lock => {
                            lock_screen(
                                &mut lock,
                                (&mut confirm, &mut elevate, &mut resolution),
                                (account, shown_name),
                                (&mut shell, &mut compositor, &mut desktop),
                                &mut server,
                                &mut sink,
                            );
                            if !lock.is_locked() {
                                idle.lock_refused(now_ns);
                            }
                        }
                        // A background session's screen is not its own to cover
                        // or to switch off.
                        IdleAction::StartScreensaver | IdleAction::SwitchDisplayOff
                            if switch.is_background() => {}
                        IdleAction::StartScreensaver => {
                            let settings = desktop.settings();
                            start_screensaver(
                                &mut saver,
                                &ScreensaverPreview::of(settings),
                                settings.backdrop,
                                (&shell, &mut compositor),
                                &mut SaverHosting {
                                    savers: &savers,
                                    reaper: &child_reaper,
                                    identity: (account, &host_name),
                                    machine: saver_machine,
                                    catalog: &wallpaper_catalog,
                                    slides: &mut saver_slides,
                                },
                                (now_ns, false),
                            );
                        }
                        IdleAction::SwitchDisplayOff => {
                            let asked = saver.switch_display_off(
                                &mut compositor,
                                display
                                    .as_mut()
                                    .map(|display| display as &mut dyn Presenter),
                            );
                            match asked {
                                Some(SwitchedOff::Blanked) if !told_unswitchable => {
                                    told_unswitchable = true;
                                    io::write_stderr_line(
                                        "desktop: this display cannot switch itself off; the screensaver is kept black instead",
                                    );
                                }
                                Some(SwitchedOff::Refused(refusal)) => {
                                    app::report(
                                        APP_NAME,
                                        format_args!(
                                            "the display would not switch off ({refusal:?}); \
                                             the screensaver is kept black instead"
                                        ),
                                    );
                                }
                                _ => {}
                            }
                        }
                    }
                }
                saver.keep_topmost(&mut compositor);
                lock.keep_topmost(&shell, &mut compositor, saver.window());
                elevate.keep_topmost(&mut compositor, saver.window());
                serve_rest(&mut shell, &mut compositor, &mut menu, &windows, now_ns);
                shell.advance_window_thumbnails(&mut compositor);
                animate(
                    &mut fade,
                    (&mut lock, &mut elevate),
                    &mut shell,
                    &desktop,
                    (&mut compositor, &pacer),
                    now_ns,
                );
                // A display switched off is presented nothing: no frame sent
                // to it could be seen, and the damage waits for it to wake.
                // Nor is a frame the lock does not cover, which would show
                // the session around it.
                if presentable(&saver, &lock, &compositor)
                    && pacer.admit(
                        now_ns,
                        compositor.has_damage() || server.has_pending_frames(),
                        display.as_ref().is_none_or(Presenter::ready),
                        compositor.refresh_rate(),
                    )
                {
                    let held = seat_held(
                        &lock,
                        (&picker, shell.router().focused()),
                        (&confirm, &elevate, &resolution),
                    );
                    latch_frames(&mut server, &mut shell, &mut compositor, &mut windows, held);
                    answer_presents(&mut server);
                    // A window first shown may have taken the keyboard.
                    reconcile_focus(
                        &mut focused,
                        &mut server,
                        &mut sink,
                        &mut shell,
                        &mut compositor,
                        &mut windows,
                        &mut picker,
                        &mut apps.service,
                        &mut menu,
                    );
                    match present(
                        &mut shell,
                        &mut compositor,
                        &mut display,
                        &mut fade,
                        &mut windows,
                        &mut menu,
                        &mut picker,
                        (&mut apps.service, &mut desktop),
                        &mut suspended,
                        TextDrawn {
                            fonts: &font_worker,
                            generation: text_drawn,
                        },
                    ) {
                        Ok(true) => saver.shown(&mut compositor),
                        Ok(false) => {}
                        Err(code) => return code,
                    }
                    pacer.presented(display.as_ref().is_some_and(|ring| !ring.ready()));
                }
                advance_saver(
                    (&mut saver, &lock),
                    (&mut shell, &mut compositor, &mut pacer),
                    (
                        display.as_ref().is_none_or(Presenter::ready),
                        server.has_pending_frames(),
                    ),
                    now_ns,
                );
                frames.maybe_send(
                    &compositor,
                    switchboard_pid,
                    frame_content(&mut windows, &server, &identity, switchboard_pid),
                    now_ns,
                    &mut RtSwitchboardMailbox,
                );
                frame_stats.maybe_publish(&compositor, now_ns, &mut frame_sink);
                tairix_rt::cachereport::publish_if_due();
                effects.seat(!switch.is_background());
                pump_private(&mut effects, &mut memory, &private_worker, now_ns);
                fetch_drawn_text(&font_worker);
                continue;
            }
            // Dispatch on the woken member's token and handle only that
            // source: `call_recv` *blocks* when nothing is pending, so a
            // seat-input wake must never touch the window endpoint (and
            // vice versa). Readiness is a non-consuming peek, so a member
            // left pending re-reports on the very next wait, and the
            // wait-set hands ready members out in turn — which is what
            // makes one source per wake safe. Were it fixed priority by
            // registration order instead, a hand on the mouse would hold
            // the seat member ready for as long as it moved and every
            // application blocked in a window call would hang until it
            // stopped.
            // Idleness is counted from real seat input: a repeat the session
            // makes up for a held key would let a stuck key hold the lock off.
            if token == SEAT_TOKEN && !repeat_due {
                idle.input(tairix_rt::clock_get());
            }
            // With no listing worker a watch's change is drained here, and
            // adopted as a worker's answer is.
            let drained_here = match token {
                PINBOARD_WATCH_TOKEN => listings.want_drain(ListingClient::Pinboard),
                _ => token
                    .checked_sub(PICKER_WATCH_BASE)
                    .and_then(|number| picker.numbered(number))
                    .is_some_and(|pick| listings.want_drain(ListingClient::Picker(pick))),
            };
            if token == WINDOW_TOKEN {
                // Serve the pending window request. Every outcome — including
                // a malformed request — is a well-formed typed reply, so no
                // caller is ever left parked; a call withdrawn since the wake,
                // or a recv error, drops the wake and re-parks.
                if let Ok(Some(ServedCall { ticket, len })) =
                    tairix_rt::call_recv_ready(WINDOW_ENDPOINT, &mut request)
                {
                    // Read before the bridge borrows the picker: a menu may
                    // not be drawn over a lock screen or a picker with the
                    // keyboard, and an accepted open is answered `SeatBusy`
                    // instead.
                    let seat_held = seat_held(
                        &lock,
                        (&picker, shell.router().focused()),
                        (&confirm, &elevate, &resolution),
                    );
                    let outcome = {
                        let mut bridge = ShellWindowHost {
                            shell: &mut shell,
                            compositor: &mut compositor,
                            windows: &mut windows,
                            picker: &mut picker,
                            apps: &mut apps.service,
                            menu: &mut menu,
                            seat_held,
                            screensaver: Some(ScreensaverServe {
                                settings: desktop.settings(),
                                owns_screen: !switch.is_background(),
                                savers: &savers,
                            }),
                            relay: &mut RtDocumentRelay,
                            wallpapers: &mut Gallery {
                                catalog: &wallpaper_catalog,
                                savers: &savers,
                                desk: &wallpapers,
                            },
                            cursor_sets: &cursor_set_names,
                            clipboard: &mut clipboard,
                            authorise: Some(AuthoriseDesk {
                                prompt: &mut elevate,
                                elevator: &mut elevator,
                            }),
                        };
                        server.serve(
                            &mut bridge,
                            &mut sink,
                            &mut identity,
                            ticket,
                            &request[..len],
                            &mut reply,
                        )
                    };
                    // A window opened by this pass wears the icon of the
                    // application the kernel says owns it. It runs here, not
                    // in the bridge, because the attested-caller table is
                    // borrowed while a request is served.
                    resolve_window_identities(
                        &mut shell,
                        &mut compositor,
                        &mut windows,
                        &programs.bundles,
                        |owner| identity.app_of(owner),
                    );
                    // A present replacing a frame not yet shown is answered
                    // once that frame is, among the engine's answers.
                    if let tairix_window::Served::Reply(n) = outcome {
                        reply_call(WINDOW_ENDPOINT, ticket, &reply[..n]);
                    }
                    answer_presents(&mut server);
                    // Settings reading the screensavers to the end of the list
                    // reads the stores again, for its next listing.
                    if shell.take_savers_listed() && scan_savers(&saver_files, &mut scan_owed) {
                        collect_saver_files(&saver_files, &mut saver, &mut savers, &mut compositor);
                    }
                    // The desktop's Settings application asked to see a
                    // screensaver; it is shown once the request that asked is
                    // answered.
                    if let Some(asked) = shell.take_screensaver_preview() {
                        start_screensaver(
                            &mut saver,
                            &asked,
                            desktop.settings().backdrop,
                            (&shell, &mut compositor),
                            &mut SaverHosting {
                                savers: &savers,
                                reaper: &child_reaper,
                                identity: (account, &host_name),
                                machine: saver_machine,
                                catalog: &wallpaper_catalog,
                                slides: &mut saver_slides,
                            },
                            (tairix_rt::clock_get(), true),
                        );
                        saver.keep_topmost(&mut compositor);
                    }
                    // The desktop's Settings application asked for the lock;
                    // it is put up once the request that asked is answered.
                    if shell.take_lock_request() {
                        lock_screen(
                            &mut lock,
                            (&mut confirm, &mut elevate, &mut resolution),
                            (account, shown_name),
                            (&mut shell, &mut compositor, &mut desktop),
                            &mut server,
                            &mut sink,
                        );
                    }
                    // A request that moved real geometry — a size-state
                    // change — owes its app the new extent, and the host
                    // could not send it while the engine held the borrow.
                    deliver_owed_events(
                        &mut server,
                        &mut sink,
                        &mut shell,
                        &mut compositor,
                        &mut windows,
                        &mut picker,
                        &mut apps.service,
                        &mut menu,
                    );
                    // A window that named the folder a save was dropped on
                    // has the file opened there now.
                    step_pick(
                        picker.take_owed_step(),
                        &Openers {
                            identity: &identity,
                            programs: &programs,
                        },
                        &listings,
                        &mut server,
                        &mut sink,
                        &mut shell,
                        &mut compositor,
                        &mut windows,
                        &mut picker,
                        &mut apps,
                        &mut menu,
                    );
                    reconcile_focus(
                        &mut focused,
                        &mut server,
                        &mut sink,
                        &mut shell,
                        &mut compositor,
                        &mut windows,
                        &mut picker,
                        &mut apps.service,
                        &mut menu,
                    );
                    // A chain this pass brought up has to reach the screen,
                    // and one it displaced has to be answered. Both run here
                    // rather than in the bridge, for the reason the identity
                    // pass above does: the engine holds the borrow the
                    // delivery needs while a request is being served.
                    // A chain displaced here is dismissed, never chosen — a
                    // row is chosen only where a seat event reaches the chain,
                    // which is the seat's own drain — so nothing lands in the
                    // bar's answer sink for this branch to route.
                    answer_menu_chain(
                        &mut menu,
                        &mut shell,
                        &mut compositor,
                        &mut windows,
                        &mut server,
                        &mut sink,
                        &mut picker,
                        &mut apps.service,
                        &identity,
                        &mut DesktopMenuDesk {
                            pinboard: &mut pinboard,
                            wallpapers: &wallpapers,
                            publisher: &publisher,
                            catalogs: &catalogs,
                            files: &listings,
                            desktop: &mut desktop,
                            launched: &mut launched,
                            programs: &mut programs,
                            answered: &mut answered,
                            capture: &mut capture,
                        },
                        tairix_rt::clock_get(),
                    );
                }
            } else if token == NOTIFY_TOKEN {
                // Serve a pending notification request: attest the producer
                // from the kernel (never the wire), decode fail-closed, relay
                // the raise/clear to the taskbar model, and answer with the
                // shared status reply. A malformed request or an unattestable
                // caller is a typed refusal, so no producer is left parked.
                let mut request = [0u8; NOTIFY_MAX_REQUEST];
                if let Ok(Some(ServedCall { ticket, len })) =
                    tairix_rt::call_recv_ready(NOTIFY_ENDPOINT, &mut request)
                {
                    let result = serve_notify(
                        &mut shell,
                        &mut compositor,
                        &desktop.settings().notifications,
                        ticket,
                        &request[..len],
                    );
                    if let Ok(Some(event)) = result {
                        // Refused or silent, the answer is the authority's
                        // to record; the notification is shown either way.
                        let _ = cue_posts.post(
                            &mut tairix_soundtheme::cue::RtCuePost::new(set, CUE_TOKEN),
                            event,
                            tairix_soundtheme::Answer::Started,
                            tairix_soundtheme::CUE_ANSWER_DEADLINE_NS,
                        );
                    }
                    let reply = encode_status_reply(result.map(drop));
                    reply_call(NOTIFY_ENDPOINT, ticket, &reply);
                }
            } else if token == SWITCHBOARD_TOKEN {
                // Serve a pending monitor call: attest that the caller is
                // the Switchboard child this session spawned (never the
                // wire), decode fail-closed, apply it, and answer. A
                // foreign caller, a malformed frame, or an owner this
                // session cannot act on is a typed refusal, so no caller
                // is left parked.
                let mut request = [0u8; SWITCHBOARD_MAX_REQUEST];
                if let Ok(Some(ServedCall { ticket, len })) =
                    tairix_rt::call_recv_ready(SWITCHBOARD_ENDPOINT, &mut request)
                {
                    let result = serve_switchboard(
                        SwitchboardServe {
                            shell: &mut shell,
                            compositor: &mut compositor,
                            launched: &mut launched,
                            owner_windows: &SessionOwnerWindows {
                                server: &server,
                                windows: &windows,
                                identity: &identity,
                            },
                            relaunch:
                                &mut |launched: &mut LaunchTable, run_path: &str, label: &str| {
                                    let _ = record_launch(
                                        launched,
                                        spawn_app(run_path.as_bytes(), &[], None),
                                        label,
                                        run_path,
                                    );
                                },
                            self_proc_id: self_origin.proc_id(),
                            machine: &mut |report, compositor| {
                                let drawn = saver.wants_machine_reports();
                                let now_ns = tairix_rt::clock_get();
                                if let Some(end) =
                                    saver.adopt_machine_report(report, now_ns, compositor)
                                {
                                    report_saver_end(&end);
                                }
                                drawn
                            },
                        },
                        ticket,
                        &request[..len],
                    );
                    // A publish is the proof an instance is up and
                    // draining, and names which one: the session tracks
                    // the attested publisher from here, so a press that
                    // arrived before it was up has an instance to open on
                    // now and every later command goes to the instance
                    // that answered rather than to a guess.
                    if let Ok(SwitchboardOutcome::Published { publisher, .. }) = result {
                        switchboard_pid = Some(publisher);
                        // The publish is also what makes the instance willing
                        // to *take* a command, so the roster it needs to draw
                        // task icons goes out here rather than waiting for the
                        // next window to open or close.
                        owner_bundles.attest(publisher);
                        owner_bundles.publish(
                            Some(publisher),
                            &apps.strip,
                            &mut RtSwitchboardMailbox,
                        );
                        deliver_pending_open(
                            &mut pending_open,
                            publisher,
                            &mut RtSwitchboardMailbox,
                        );
                        machine_watch.attest(publisher, &mut RtSwitchboardMailbox);
                    }
                    let mut reply = [0u8; SWITCHBOARD_PUBLISH_REPLY_LEN];
                    let len = encode_switchboard_reply(&result, &mut reply);
                    reply_call(SWITCHBOARD_ENDPOINT, ticket, &reply[..len]);
                }
            } else if token == PINBOARD_TOKEN {
                // Serve a pending pinboard apply: attest that the caller
                // runs as this session's own user from the kernel (never
                // the wire), parse the carried document with the one
                // settings engine, and put it through the very same
                // persist-then-adopt path the backdrop menu uses, so no two
                // routes can diverge. A foreign caller, a malformed
                // frame, an unusable document, or a refused write is a
                // typed refusal stated on `stderr`, so no caller is left
                // parked and the desktop keeps the settings it had. The
                // reply waits for the store, not for this loop: the call is
                // answered by whichever path learns the outcome.
                let mut request = [0u8; PINBOARD_MAX_REQUEST];
                if let Ok(Some(ServedCall { ticket, len })) =
                    tairix_rt::call_recv_ready(PINBOARD_ENDPOINT, &mut request)
                {
                    // Merged over what is on its way to the store, so an apply
                    // arriving before the last one has landed does not undo it.
                    let attested = attest_pinboard(
                        self_origin.uid(),
                        (
                            &publisher.base(desktop.settings()),
                            &publisher.files_base(shell.files()),
                            shell.zones(),
                        ),
                        ticket,
                        &request[..len],
                    );
                    // While the user is deciding about a resolution, one change
                    // is in hand: another could only race the answer.
                    let deciding = resolution.is_open() || publisher.keeping();
                    let asked = match attested {
                        Err(err) => {
                            reply_pinboard(ticket, Err(err));
                            None
                        }
                        Ok(call) if deciding && call.changes_settings() => {
                            reply_pinboard(ticket, Err(Errno::Busy));
                            None
                        }
                        Ok(PinboardCall::Sound(call)) => {
                            serve_sound_call(
                                &call,
                                ticket,
                                (&mut effects, &mut memory),
                                &private_worker,
                                &sound_themes,
                                &mut picker,
                                &mut shell,
                                &mut compositor,
                            );
                            None
                        }
                        Ok(PinboardCall::ApplyFiles(files)) => {
                            if let Some(answer) = publisher.submit(
                                publisher.base(desktop.settings()),
                                files,
                                Some(ticket),
                            ) {
                                if adopt_publish(
                                    answer,
                                    &publisher,
                                    &mut pinboard,
                                    &wallpapers,
                                    &mut desktop,
                                    &mut shell,
                                    &mut compositor,
                                    tairix_rt::clock_get(),
                                ) {
                                    shell.present_desktop(&mut compositor, &desktop);
                                }
                            }
                            None
                        }
                        Ok(PinboardCall::Apply(applied)) => apply_pinboard(
                            applied,
                            &publisher,
                            &mut pinboard,
                            &wallpapers,
                            &mut desktop,
                            &mut shell,
                            &mut compositor,
                            lock.is_locked() || saver.is_shown(),
                            ticket,
                        ),
                    };
                    if let Some(ask) = asked {
                        let placements = begin_trial(
                            ask,
                            &mut Resize {
                                compositor: &mut compositor,
                                shell: &mut shell,
                                desktop: &mut desktop,
                                pinboard: &mut pinboard,
                                wallpapers: &wallpapers,
                                windows: &mut windows,
                                menu: &mut menu,
                                pointer: &mut pointer,
                                prompts: (&mut confirm, &mut elevate, &mut resolution),
                                saver: &mut saver,
                                lock: &mut lock,
                                idle: &mut idle,
                                memory_bytes,
                            },
                        );
                        // A trial that could not be put to the user is
                        // reverted at once, its placements with it.
                        if resolution.is_open() {
                            trial_placements = placements;
                        } else {
                            owed_placements = placements;
                        }
                        sized_for = None;
                        deliver_owed_events(
                            &mut server,
                            &mut sink,
                            &mut shell,
                            &mut compositor,
                            &mut windows,
                            &mut picker,
                            &mut apps.service,
                            &mut menu,
                        );
                    }
                }
            } else if token == HOLDBACK_TOKEN {
                // An app drained its full event mailbox, or the session's own
                // budget freed, so what the session owes can go out; or the
                // pause after a send the kernel had no memory for is over. The
                // room member is armed exactly while a destination waits on its
                // room, so this never runs on a wake nobody asked for — the
                // desktop never polls for capacity.
                answer_presents(&mut server);
                for endpoint in sink.flush() {
                    // The send proved this owner gone before its exit was
                    // reaped. Its windows go with it, exactly as a refused
                    // direct send tears them down.
                    if let Some(client) = identity.take_by_event_endpoint(endpoint) {
                        let mut bridge = ShellWindowHost {
                            shell: &mut shell,
                            compositor: &mut compositor,
                            windows: &mut windows,
                            picker: &mut picker,
                            apps: &mut apps.service,
                            menu: &mut menu,
                            seat_held: true,
                            screensaver: None,
                            relay: &mut RtDocumentRelay,
                            wallpapers: &mut Gallery {
                                catalog: &wallpaper_catalog,
                                savers: &savers,
                                desk: &wallpapers,
                            },
                            cursor_sets: &cursor_set_names,
                            clipboard: &mut clipboard,
                            authorise: None,
                        };
                        server.client_exited(&mut bridge, client);
                        if focused.is_some_and(|id| server.owner_of(id).is_none()) {
                            focused = None;
                        }
                    }
                }
            } else if token == WORKER_TOKEN || drained_here {
                // A worker finished something. Drain the nudge bytes (the member
                // is a level-triggered peek, so anything left re-reports on the
                // next wait and nothing is lost), then offer every consumer the
                // chance to adopt what arrived. Each is a no-op unless it was the
                // one waiting, so one wake serves whichever it was. An update
                // drained here left no byte in the pipe, and reading an empty
                // one would park the loop for good.
                if token == WORKER_TOKEN {
                    worker_wake.drain();
                }
                let settings_landed = collect_publish(
                    &publisher,
                    &mut pinboard,
                    &wallpapers,
                    &mut desktop,
                    &mut shell,
                    &mut compositor,
                    tairix_rt::clock_get(),
                );
                if let Some((want, landed)) = wallpapers.take_slide() {
                    if let Err(why) = &landed {
                        app::report(APP_NAME, format_args!("a slide could not be shown: {why}"));
                    }
                    let slide = (want.index, landed.as_ref().ok());
                    let now_ns = tairix_rt::clock_get();
                    if let Some(end) =
                        saver.slide_landed(want.saver, slide, now_ns, &mut compositor)
                    {
                        report_saver_end(&end);
                    }
                }
                if let Some(loaded) = catalogs.collect() {
                    adopt_programs(loaded, &mut shell, &mut compositor, &mut programs);
                }
                // A listing landing and what the folder's watch reported change
                // the icon cells they touch and nothing behind them.
                let mut cells = damage::sink();
                let resumed =
                    desktop.resume_into(|desk| shell.desktop_layout(&compositor, desk), &mut cells);
                let papered = prepare_wallpaper(
                    &mut pinboard,
                    &wallpapers,
                    &mut shell,
                    &desktop,
                    &compositor,
                    tairix_rt::clock_get(),
                );
                if papered {
                    fade.set_awaiting_backdrop(!shell.backdrop_settled());
                }
                // The batch names which decodes came back, so each surface
                // adopts it at the granularity of the items it draws — a
                // slot, a window's identity, a bar control — rather than
                // repainting itself whole because something arrived.
                let arted = artworks.take_landed();
                if !arted.is_empty() {
                    refresh_app_strip(
                        &mut apps,
                        &mut shell,
                        &mut compositor,
                        &server,
                        &windows,
                        &identity,
                        &programs.bundles,
                    );
                    // The monitor draws these same applications against its
                    // task rows, and the bundle each runs is a fact only this
                    // session holds. Offered where the strip
                    // has just been re-resolved, so a launch is one send
                    // rather than a re-send of the whole roster per frame.
                    owner_bundles.publish(switchboard_pid, &apps.strip, &mut RtSwitchboardMailbox);
                    resolve_window_identities(
                        &mut shell,
                        &mut compositor,
                        &mut windows,
                        &programs.bundles,
                        |owner| identity.app_of(owner),
                    );
                    shell.present_icon_artwork(&mut compositor, &arted);
                    picker.artwork_landed(&arted, &mut shell, &mut compositor);
                    // An open chain re-resolves its pictures as it presents,
                    // so a sheet or row that opened before its decode landed
                    // is drawn with it now rather than on the next open.
                    if menu.is_open() {
                        present_menu_chain(&mut menu, &mut shell, &mut compositor, &windows);
                    }
                }
                let layout = |desk: &_| shell.desktop_layout(&compositor, desk);
                let moved = match listings.take_update(ListingClient::Pinboard) {
                    WatchUpdate::Quiet => false,
                    WatchUpdate::Changes(changes) => {
                        let cued = listings.invalidate_probes(desktop.folder(), &changes);
                        desktop.apply_changes(changes, layout, &mut cells) || cued
                    }
                    WatchUpdate::Rescan => desktop.relist_into(layout, &mut cells),
                    WatchUpdate::Gone => {
                        listings.unwatch(ListingClient::Pinboard);
                        desktop.relist_into(layout, &mut cells)
                    }
                };
                // Each folder the column now shows is asked about, and each cue
                // a probe batch answered is drawn.
                if listings.take_probes_landed() || resumed || moved || settings_landed {
                    desktop.resolve_occupancy(&layout(&desktop), &mut cells);
                    // The desktop is the session's one surface with folder
                    // cues, and it just asked about every folder it shows.
                    listings.sweep_probes();
                }
                // A new wallpaper replaced the ground, and a settings change
                // restyled or re-laid the layer: each of those is the whole
                // of it. A listing, a reported change or arriving artwork is
                // not — it changes the tiles and nothing behind them — so it
                // repaints the cells and leaves the ground alone.
                if papered || settings_landed {
                    shell.present_desktop(&mut compositor, &desktop);
                } else {
                    shell.mark_desktop_artwork(&compositor, &desktop, &arted, &mut cells);
                    if !cells.is_empty() {
                        shell.present_desktop_area(&mut compositor, &desktop, &cells);
                    }
                }
                // The desktop, a carried drag's cards and the picks are the
                // session's surfaces drawing thumbnail-class pictures and
                // listed bundles; asking for every one they show first is what
                // lets a pass that repainted a few of them withdraw only what
                // left the screen.
                if !arted.is_empty() {
                    shell.want_desktop_artwork(&compositor, &desktop);
                    shell.want_drag_artwork(&compositor);
                    picker.want_shown_artwork(&mut shell, &compositor);
                    artworks.sweep();
                }
                picker.resume(&mut shell, &mut compositor);
                picker.follow_each(
                    |pick| {
                        let client = ListingClient::Picker(pick);
                        let update = listings.take_update(client);
                        if matches!(update, WatchUpdate::Gone) {
                            listings.unwatch(client);
                        }
                        update
                    },
                    &mut shell,
                    &mut compositor,
                );
                // Whatever moved a carried drag's folder this turn — an answer
                // lighting it, a drop, a window it was offered to closing, the
                // screen locking — leaves the desktop showing the one it names.
                if desktop.drop_target() != shell.drag_drop_icon() {
                    light_drop_icon(&mut shell, &mut compositor, &mut desktop);
                }
                // Every file call carried out since the last wake, in the order
                // it was asked.
                while let Some(answer) = listings.collect() {
                    match answer {
                        FileAnswer::Pick {
                            pick,
                            serial,
                            opened,
                            created,
                            made,
                        } => {
                            // Made by the session, the file shows in a desktop
                            // folder holding it even where no watch says so.
                            let created_here = created.clone().filter(|_| opened.is_ok());
                            let made_here = opened.is_ok().then(|| made.clone());
                            let discard = settle_pick(
                                (pick, serial),
                                (opened, created, made),
                                &mut server,
                                &mut sink,
                                &mut shell,
                                &mut compositor,
                                &mut windows,
                                &mut picker,
                                &mut apps,
                                &mut menu,
                            );
                            let kept = made_here.filter(|_| discard.is_none());
                            if let Some(job) = discard {
                                let _ = listings.submit(job);
                            } else if let Some(path) = created_here {
                                show_unreported(
                                    Some(path.as_str()),
                                    &mut desktop,
                                    &mut shell,
                                    &mut compositor,
                                );
                            }
                            // A save dropped on the bare desktop stands where
                            // it was let go, under the name its open made.
                            land_saved(
                                &mut desktop,
                                (pick.get(), serial),
                                kept.as_deref(),
                                (&mut shell, &mut compositor),
                            );
                        }
                        FileAnswer::Folder {
                            pick,
                            serial,
                            opened,
                        } => settle_folder_pick(
                            (pick, serial),
                            opened,
                            &mut server,
                            &mut sink,
                            &mut shell,
                            &mut compositor,
                            &mut windows,
                            &mut picker,
                            &mut apps,
                            &mut menu,
                        ),
                        FileAnswer::NewFolder {
                            pick,
                            serial,
                            made,
                            path,
                        } => {
                            let shown = made.is_ok();
                            picker.folder_made(pick, serial, made, &mut shell, &mut compositor);
                            if shown {
                                show_unreported(
                                    Some(path.as_str()),
                                    &mut desktop,
                                    &mut shell,
                                    &mut compositor,
                                );
                            }
                        }
                        FileAnswer::Discarded => {}
                        FileAnswer::Desktop(done) => {
                            settle_desktop_call(
                                done,
                                &mut desktop,
                                &mut shell,
                                &mut compositor,
                                &mut LaunchCtx {
                                    launched: &mut launched,
                                    apps: &apps.service,
                                    server: &mut server,
                                    sink: &mut sink,
                                    windows: &windows,
                                    identity: &identity,
                                },
                            );
                        }
                    }
                }
                // A read of the mount table refused while the worker was full
                // is asked again now it has taken some.
                ask_for_mounts(&listings, &mut desktop, &mut shell, &mut compositor);
                while let Some(done) = wallpapers.take_preview() {
                    settle_wallpaper_preview(
                        done,
                        &mut server,
                        &mut sink,
                        &mut shell,
                        &mut compositor,
                        &mut windows,
                        &mut picker,
                        &mut apps,
                        &mut menu,
                    );
                }
            } else if token == PRESSURE_TOKEN {
                // The machine's memory-pressure band moved. Read it and, if
                // it really changed, give back whatever the new band says a
                // disposable-UI cache may keep — at the moment pressure
                // rises, not at whatever later frame happens to touch a
                // cache. The desktop's rasterised pixels are among the first
                // memory the system reclaims, ahead of clean file data and
                // well ahead of compressing anyone's anonymous pages.
                //
                // The cursor, glyphs, and window furniture remain correct
                // throughout: a dropped entry is simply rendered again on
                // demand, so this costs rendering work and never a wrong
                // pixel. Nothing is repainted here, and a band that demands
                // nothing releases nothing but frosts the window stack no
                // longer earns, so a wake the desktop has already acted on is
                // almost free.
                //
                // Window *content* is the one thing the desktop cannot
                // re-render itself, so a *visible* window whose pixels the
                // same trim released is asked to present again straight away.
                // A hidden one is told it may let go of its own copies
                // instead, and asked when it is next shown: presenting it now
                // would spend the memory the release recovered on pixels
                // nobody can see.
                if tairix_procinfo::pressure::refresh() {
                    wallpapers.adopt_band(
                        preparers,
                        memory_total,
                        tairix_rt::pressure::gauge().band(),
                    );
                    artworks.adopt_band();
                    let _ = shell.trim_caches(&mut compositor);
                    tairix_font::trim_glyph_cache();
                    let band = tairix_rt::pressure::gauge().band();
                    // A picker the user cannot see is painted again from its
                    // state once it shows.
                    if band != tairix_reclaim::PressureBand::Normal {
                        let _ = picker.release_hidden(&mut compositor);
                    }
                    if let Some(end) = saver.pressure(band, tairix_rt::clock_get(), &mut compositor)
                    {
                        report_saver_end(&end);
                    }
                    deliver_released_notices(
                        &mut server,
                        &mut sink,
                        &mut shell,
                        &mut compositor,
                        &mut windows,
                        &mut picker,
                        &mut apps.service,
                        &mut menu,
                    );
                    // A band that refused to keep a decode may now allow it:
                    // where the cache would keep one, it is asked for again,
                    // here on the band's own wake rather than by every
                    // repaint in between.
                    artworks.retry_declined(|bytes| shell.holds_artwork(bytes));
                    deliver_pending_redraws(
                        &mut server,
                        &mut sink,
                        &mut shell,
                        &mut compositor,
                        &mut windows,
                        &mut picker,
                        &mut apps.service,
                        &mut menu,
                    );
                }
            } else if token == SOUND_TOKEN || token == AUDIO_DEVICES_TOKEN {
                if token == SOUND_TOKEN {
                    sound_worker.wake().drain();
                } else {
                    sound.refresh();
                }
                pump_sound(
                    &mut sound,
                    &sound_worker,
                    &mut cue_posts,
                    set,
                    &publisher,
                    &mut pinboard,
                    &wallpapers,
                    &mut desktop,
                    &mut shell,
                    &mut compositor,
                );
            } else if token == PRIVATE_TOKEN || token == CUE_AUTHORITY_TOKEN {
                if token == PRIVATE_TOKEN {
                    private_worker.wake().drain();
                } else {
                    effects.authority_is(cue_authority());
                }
                effects.seat(!switch.is_background());
                pump_private(
                    &mut effects,
                    &mut memory,
                    &private_worker,
                    tairix_rt::clock_get(),
                );
            } else if token == MOUNTS_TOKEN {
                listings.reroute();
                artworks.reroute();
                show_unreported(None, &mut desktop, &mut shell, &mut compositor);
                desktop.mounts_moved();
                ask_for_mounts(&listings, &mut desktop, &mut shell, &mut compositor);
                serve_held(
                    (&held_writes, &mut held_questions),
                    true,
                    &mut windows,
                    &mut shell,
                    &mut compositor,
                );
            } else if token == HELD_WRITES_TOKEN {
                held_writes.wake().drain();
                serve_held(
                    (&held_writes, &mut held_questions),
                    false,
                    &mut windows,
                    &mut shell,
                    &mut compositor,
                );
            } else if token == AUDIO_CAPTURE_TOKEN {
                shell.set_sound(&mut compositor, sound.captures(live_captures()));
            } else if token == DISPLAY_OUTPUT_TOKEN {
                // A background session holds no frames, and reads the output
                // afresh when it comes back.
                let forgotten = suspended.take().unwrap_or(false);
                if !switch.is_background() {
                    let followed = follow_output(
                        forgotten,
                        (&mut display, set),
                        &mut Resize {
                            compositor: &mut compositor,
                            shell: &mut shell,
                            desktop: &mut desktop,
                            pinboard: &mut pinboard,
                            wallpapers: &wallpapers,
                            windows: &mut windows,
                            menu: &mut menu,
                            pointer: &mut pointer,
                            prompts: (&mut confirm, &mut elevate, &mut resolution),
                            saver: &mut saver,
                            lock: &mut lock,
                            idle: &mut idle,
                            memory_bytes,
                        },
                    );
                    match followed {
                        Err(code) => return code,
                        Ok(Followed::Settled) => {}
                        Ok(Followed::Moved(mode)) => {
                            sized_for = Some((
                                desktop.settings().resolution,
                                (mode.width_px, mode.height_px),
                            ));
                            deliver_owed_events(
                                &mut server,
                                &mut sink,
                                &mut shell,
                                &mut compositor,
                                &mut windows,
                                &mut picker,
                                &mut apps.service,
                                &mut menu,
                            );
                        }
                    }
                }
            } else if token == CUE_TOKEN {
                let pushed = cue_posts.collect(
                    &mut tairix_soundtheme::cue::RtCuePost::new(set, CUE_TOKEN),
                    |_, _| {},
                );
                if let Some(result) = pushed {
                    let now = tairix_rt::clock_get();
                    refused_push(effects.pushed(now, result));
                    pump_private(&mut effects, &mut memory, &private_worker, now);
                }
            } else if token == CHILD_TOKEN {
                // Reap every exited child in one wake and act on each: a child
                // whose asynchronous load was refused exits with a reserved
                // `LOAD_*` status (the load ran on the child's own task, so
                // the refusal arrives here, not at `spawn`), which is reported
                // fail-loud on `stderr` named by its launcher label; and every
                // reaped child — refused or clean — has its windows torn down
                // (the kernel already reclaimed its port and shm). Draining
                // fully is safe and never busy-waits: the non-blocking `wait`
                // yields nothing once no zombie remains. The whole
                // reap/report/teardown flow is the shared, host-tested
                // `reap_launched`.
                let mut file_manager_ended = None;
                reap_launched(
                    &mut launched,
                    || {
                        // Placeholder the kernel overwrites on a successful
                        // reap; only the pid and status are needed.
                        let mut status = WaitStatus::Exited(0);
                        let pid = tairix_rt::wait(WAIT_PID_ANY, &mut status, WaitFlags::NONBLOCK);
                        if pid > 0 {
                            #[allow(clippy::cast_sign_loss)] // guarded by `pid > 0`.
                            let pid = pid as u64;
                            recorder.reaped(pid, status);
                            Some((pid, status))
                        } else {
                            None
                        }
                    },
                    app::report_line,
                    |pid, ended| {
                        if let Some(app) = ended.filter(|app| app.run_path == FILES_RUN_PATH) {
                            file_manager_ended = Some(app.started_ns);
                        }
                        if let Some(client) = identity.take_by_pid(pid) {
                            let mut bridge = ShellWindowHost {
                                shell: &mut shell,
                                compositor: &mut compositor,
                                windows: &mut windows,
                                picker: &mut picker,
                                apps: &mut apps.service,
                                menu: &mut menu,
                                seat_held: true,
                                screensaver: None,
                                relay: &mut RtDocumentRelay,
                                wallpapers: &mut Gallery {
                                    catalog: &wallpaper_catalog,
                                    savers: &savers,
                                    desk: &wallpapers,
                                },
                                cursor_sets: &cursor_set_names,
                                clipboard: &mut clipboard,
                                authorise: None,
                            };
                            server.client_exited(&mut bridge, client);
                            if focused.is_some_and(|id| server.owner_of(id).is_none()) {
                                focused = None;
                            }
                        }
                        // A launched app that raised notifications and then
                        // exited can no longer clear them; drop them here so a
                        // dead producer leaves no stuck notification — the
                        // notification counterpart of the window teardown
                        // above, run for every reaped child, windowed or not.
                        shell.clear_pid_notifications(&mut compositor, pid);
                        // A reaped child is gone, not hung: drop its delivery
                        // evidence so a recycled task id starts clean. And a
                        // reaped Switchboard can publish nothing more — clear
                        // its feeds so the bar states nothing rather than
                        // freezing a dead service's last reading.
                        sink.forget_owner(pid);
                        if switchboard_pid == Some(pid) {
                            switchboard_pid = None;
                            shell.set_tray_summary(&mut compositor, None);
                            shell.set_network_report(&mut compositor, None);
                            if let Some(end) =
                                saver.machine_unmonitored(tairix_rt::clock_get(), &mut compositor)
                            {
                                report_saver_end(&end);
                            }
                        }
                        machine_watch.forget(pid);
                    },
                );
                // The desktop's file manager never quits, so one that ended is
                // started again — unless a folder opened meanwhile already did,
                // or the session is itself leaving.
                if let Some(started_ns) = file_manager_ended.filter(|_| {
                    departure.is_none() && launched.running_from(FILES_RUN_PATH).is_none()
                }) {
                    if file_manager.ended(started_ns, tairix_rt::clock_get()) {
                        spawn_files(&mut launched);
                    } else {
                        app::report(
                            APP_NAME,
                            format_args!(
                                "the file manager keeps stopping as it starts; opening a folder \
                                 on the desktop starts it again"
                            ),
                        );
                    }
                }
            } else if token == DISPLAY_EVENT_TOKEN {
                // A latch frees the frame a held present waits for; the
                // frame path below presents it.
                if let Some(ring) = display.as_mut() {
                    if ring.take_events() && saver.scans_out() {
                        saver.latched(&mut compositor, ring);
                    }
                }
            } else if token == WAKE_TOKEN {
                // The session authority speaking to this desktop: it is the
                // foreground session again, or the authority is going away
                // and a session it can no longer reach must not be left
                // stranded. The sender is the kernel's account of who sent
                // it, never a claim on the wire; a message from anyone else,
                // or one that does not decode, is dropped with its reason
                // stated and acted on by nothing.
                let Some(wake) = switch.wake_endpoint() else {
                    continue;
                };
                let mut message = [0u8; SESSION_WAKE_LEN];
                let mut sender = [0u8; ORIGIN_WIRE_LEN];
                let Ok(len) = tairix_rt::ipc_recv(wake, &mut message, &mut sender) else {
                    continue;
                };
                let Ok(origin) = Origin::from_bytes(&sender) else {
                    io::write_stderr_line("desktop: dropped an unattested session wake");
                    continue;
                };
                match switch.classify(&message[..len], &origin) {
                    Ok(SessionWake::Foreground) => {
                        // A refusal of the frames given up belongs to the
                        // screen this session left; the resume reads the
                        // output afresh.
                        suspended = None;
                        let resumed = switch.resume(&mut SessionReturn {
                            display: &mut display,
                            fade: &mut fade,
                            set,
                            parts: Resize {
                                compositor: &mut compositor,
                                shell: &mut shell,
                                desktop: &mut desktop,
                                pinboard: &mut pinboard,
                                wallpapers: &wallpapers,
                                windows: &mut windows,
                                menu: &mut menu,
                                pointer: &mut pointer,
                                prompts: (&mut confirm, &mut elevate, &mut resolution),
                                saver: &mut saver,
                                lock: &mut lock,
                                idle: &mut idle,
                                memory_bytes,
                            },
                        });
                        let output = match resumed {
                            Ok(output) => output,
                            Err(failure) => {
                                app::report(
                                    APP_NAME,
                                    format_args!("{} ({:?})", failure.reason(), failure.errno()),
                                );
                                shell.teardown(&mut compositor);
                                return EXIT_RESUME_FAILED;
                            }
                        };
                        // The cue authority dropped this session's sound
                        // choices with its hold, so taking the seat back owes
                        // them anew.
                        effects.seat(false);
                        effects.seat(true);
                        pump_private(
                            &mut effects,
                            &mut memory,
                            &private_worker,
                            tairix_rt::clock_get(),
                        );
                        // Idleness starts again, so the screen does not blank
                        // or lock at once.
                        idle.input(tairix_rt::clock_get());
                        if let Some(mode) = output {
                            // The windows the resume fitted to the screen are
                            // owed their new extents.
                            sized_for = Some((
                                desktop.settings().resolution,
                                (mode.width_px, mode.height_px),
                            ));
                            deliver_owed_events(
                                &mut server,
                                &mut sink,
                                &mut shell,
                                &mut compositor,
                                &mut windows,
                                &mut picker,
                                &mut apps.service,
                                &mut menu,
                            );
                        }
                    }
                    Ok(SessionWake::End) => {
                        io::write_stderr_line(
                            "desktop: the login service is going away; ending this session",
                        );
                        departure.get_or_insert_with(|| {
                            Departure::begin(tairix_rt::clock_get(), EXIT_AUTHORITY_GONE)
                        });
                    }
                    Err(refusal) => {
                        app::report(APP_NAME, refusal.reason());
                    }
                }
            } else if token == SEAT_TOKEN && saver.is_shown() {
                // The waking gesture reaches nothing behind the screensaver;
                // the next input goes to a lock, if one came up underneath.
                let now_ns = tairix_rt::clock_get();
                let drained = drain_away(
                    &mut Seat {
                        shell: &mut shell,
                        compositor: &mut compositor,
                        menu: &mut menu,
                        lock: &mut lock,
                        clipboard: &mut clipboard,
                        capture: &mut capture,
                    },
                    &mut pointer,
                    &mut keyboard,
                    now_ns,
                );
                let waking = match drained {
                    Ok(waking) => waking,
                    Err(err) => return drain_fault(&mut shell, &mut compositor, err),
                };
                // The gesture that wakes the screen reaches nothing, a press
                // of Ctrl included.
                let _ = keyboard.take_tap();
                // A preview's first moment of motion leaves it up: the hand
                // that asked for it is still on the mouse.
                let dismissed = saver.woken_by(waking, now_ns).then(|| {
                    saver.dismiss(
                        &mut compositor,
                        display
                            .as_mut()
                            .map(|display| display as &mut dyn Presenter),
                    )
                });
                match dismissed {
                    Some(Ok(_)) => {
                        told_unwakeable = false;
                        wallpapers.forget_slides();
                        // The pointer comes back in the shape of whatever it
                        // is over now.
                        shell.refresh_cursor(&mut compositor);
                    }
                    // Still dark: the next input asks again.
                    Some(Err(refusal)) if !told_unwakeable => {
                        told_unwakeable = true;
                        app::report(
                            APP_NAME,
                            format_args!("the display would not switch back on ({refusal:?})"),
                        );
                    }
                    Some(Err(_)) | None => {}
                }
            } else if token == ELEVATE_TOKEN {
                elevate_worker.wake().drain();
                let held = (&mut elevate, &mut recorder);
                adopt_elevations(&mut elevator, held, &shell, &mut compositor);
            } else if token == FONT_TOKEN {
                // What landed is laid out again at the end of this turn, where
                // every surface is brought up to date before the frame.
                font_worker.wake().drain();
                let _ = font_worker.collect();
            } else if token == SAVER_READ_TOKEN {
                if let Some(instance) = saver.hosting() {
                    let mut errands = RtSaverErrands {
                        saver: instance,
                        fonts: &saver_fonts,
                        files: &saver_files,
                        wallpapers: &wallpapers,
                        slides: (&wallpaper_catalog, &saver_slides, compositor.screen_rect()),
                        civil: shell.clock().civil(),
                        owed: OwedErrands::default(),
                    };
                    // A saver draws straight into the display once the screen
                    // has settled from its own arrival, never while dark.
                    let straight = (fade.settled() && !saver.is_dark())
                        .then_some(display.as_mut())
                        .flatten()
                        .map(|ring| ring as &mut dyn Presenter);
                    let heard = saver.on_readable(
                        tairix_rt::clock_get(),
                        (&mut compositor, straight),
                        &mut errands,
                    );
                    let owed = errands.owed;
                    if let Some(end) = heard {
                        report_saver_end(&end);
                    }
                    settle_errands(
                        &owed,
                        instance,
                        &mut saver,
                        (&saver_fonts, &saver_files),
                        &mut savers,
                        &mut compositor,
                    );
                }
            } else if token == SAVER_WRITE_TOKEN {
                if let Some(end) = saver.on_writable(tairix_rt::clock_get(), &mut compositor) {
                    report_saver_end(&end);
                }
            } else if token == RECORDER_READ_TOKEN {
                if let Some(recording) = recorder.recording.as_mut() {
                    recording.on_readable();
                }
            } else if token == RECORDER_WRITE_TOKEN {
                if let Some(recording) = recorder.recording.as_mut() {
                    recording.on_writable();
                }
            } else if token == SAVER_FONT_TOKEN {
                saver_fonts.wake().drain();
                collect_saver_fonts(&saver_fonts, &mut saver, &mut compositor);
            } else if token == CAPTURE_FILE_TOKEN {
                // Adopted by the capture's own turn, before the next wait.
                capture_files.wake().drain();
            } else if token == SAVER_FILE_TOKEN {
                saver_files.wake().drain();
                collect_saver_files(&saver_files, &mut saver, &mut savers, &mut compositor);
                if scan_owed && scan_savers(&saver_files, &mut scan_owed) {
                    collect_saver_files(&saver_files, &mut saver, &mut savers, &mut compositor);
                }
            } else if token == REAPED_TOKEN {
                child_reaper.collect();
            } else if token == SAVER_STORE_TOKEN {
                if scan_savers(&saver_files, &mut scan_owed) {
                    collect_saver_files(&saver_files, &mut saver, &mut savers, &mut compositor);
                }
            } else if token == MACHINE_ZONE_WATCH_TOKEN || token == MACHINE_ZONE_TOKEN {
                // A change is drained and the file read on the worker; with no
                // worker it ran here and its answer is already landed.
                if token == MACHINE_ZONE_TOKEN {
                    zone_worker.wake().drain();
                } else {
                    let _ = zone_worker.submit(());
                }
                while let Some(answer) = zone_worker.collect() {
                    if let Some(read) = answer {
                        adopt_machine_zone(read, desktop.settings(), &mut shell, &mut compositor);
                    }
                }
            } else if token == UNLOCK_TOKEN || (token == SEAT_TOKEN && lock.is_locked()) {
                if token == UNLOCK_TOKEN {
                    unlock_worker.wake().drain();
                }
                let now = tairix_rt::clock_get();
                let mut holder = Seat {
                    shell: &mut shell,
                    compositor: &mut compositor,
                    menu: &mut menu,
                    lock: &mut lock,
                    clipboard: &mut clipboard,
                    capture: &mut capture,
                };
                // The lock's surface only hides the session; this drain is
                // what keeps the seat's input from reaching it. An answer
                // already back — carried here for want of a thread — is
                // adopted on the same turn.
                let drained = if token == SEAT_TOKEN {
                    drain_locked(&mut holder, &mut pointer, &mut keyboard, &mut unlocker, now)
                } else {
                    Ok(())
                }
                .and_then(|()| {
                    adopt_unlocks(
                        &mut holder,
                        (&mut pointer, &mut keyboard),
                        &mut unlocker,
                        now,
                    )
                });
                if let Err(err) = drained {
                    return drain_fault(&mut shell, &mut compositor, err);
                }
            } else if token == SEAT_TOKEN && elevate.is_up() {
                // The authorisation prompt's sheet only shows the question;
                // this drain is what keeps the seat's input from reaching
                // anything behind it.
                let now = tairix_rt::clock_get();
                let mut holder = Seat {
                    shell: &mut shell,
                    compositor: &mut compositor,
                    menu: &mut menu,
                    lock: &mut lock,
                    clipboard: &mut clipboard,
                    capture: &mut capture,
                };
                match drain_authorising(
                    &mut holder,
                    (&mut pointer, &mut keyboard),
                    &mut elevate,
                    &mut elevator,
                    now,
                ) {
                    Ok(outcome) => report_elevation(outcome),
                    Err(err) => return drain_fault(&mut shell, &mut compositor, err),
                }
                let held = (&mut elevate, &mut recorder);
                adopt_elevations(&mut elevator, held, &shell, &mut compositor);
            } else if token == SEAT_TOKEN {
                // One wake is one instant: its time-driven gestures (a held
                // capsule press) resolve against the clock read here.
                let drained = seat_drain.wake(
                    &mut Seat {
                        shell: &mut shell,
                        compositor: &mut compositor,
                        menu: &mut menu,
                        lock: &mut lock,
                        clipboard: &mut clipboard,
                        capture: &mut capture,
                    },
                    &mut pointer,
                    &mut keyboard,
                    &mut SessionRoute {
                        publisher: &publisher,
                        catalogs: &catalogs,
                        files: &listings,
                        pinboard: &mut pinboard,
                        wallpapers: &wallpapers,
                        desktop: &mut desktop,
                        windows: &mut windows,
                        focused: &mut focused,
                        server: &mut server,
                        sink: &mut sink,
                        picker: &mut picker,
                        confirm: &mut confirm,
                        elevate: &mut elevate,
                        elevator: &mut elevator,
                        resolution: &mut resolution,
                        account,
                        shown_name,
                        identity: &identity,
                        launched: &mut launched,
                        apps: &mut apps,
                        switchboard_pid: &mut switchboard_pid,
                        pending_open: &mut pending_open,
                        programs: &mut programs,
                        switch: &mut switch,
                        display: &mut display,
                        fade: &mut fade,
                        set,
                        sound: &mut sound,
                        sound_worker: &sound_worker,
                        cue_posts: &mut cue_posts,
                        system: &system,
                        memory: &mut memory.launcher,
                    },
                    tairix_rt::clock_get(),
                );
                let carried = match drained {
                    Ok(SeatWake::Served { carried }) => carried,
                    // The screen belongs to somebody else now: nothing more of
                    // this wake is applied, and nothing is drawn.
                    Ok(SeatWake::SteppedAside) => continue,
                    Ok(SeatWake::EndSession) => {
                        departure.get_or_insert_with(|| {
                            Departure::begin(tairix_rt::clock_get(), EXIT_LOGGED_OUT)
                        });
                        false
                    }
                    Err(err) => return drain_fault(&mut shell, &mut compositor, err),
                };
                // The pointer on a cursor plane follows the batch now, ahead
                // of any frame: moving it presents nothing. One carrying what
                // the owed frame moves goes with that frame instead, so the
                // window it drags does not trail it.
                if let Some(ring) = display.as_mut() {
                    if !carried || !compositor.has_damage() {
                        compositor.place_pointer(ring);
                    }
                }
                // An answer already back — carried here for want of a thread —
                // is adopted on the same turn.
                let held = (&mut elevate, &mut recorder);
                adopt_elevations(&mut elevator, held, &shell, &mut compositor);
                // A window minimised while the machine was already short of
                // memory had its content released by the gesture itself, so
                // its client is told here rather than waiting for a band
                // change that may never come.
                deliver_released_notices(
                    &mut server,
                    &mut sink,
                    &mut shell,
                    &mut compositor,
                    &mut windows,
                    &mut picker,
                    &mut apps.service,
                    &mut menu,
                );
                // A window restored from the taskbar (or otherwise shown
                // again) whose content was released while it was hidden
                // has nothing to draw until its app presents, so ask now
                // rather than leaving it blank until the next wake.
                deliver_pending_redraws(
                    &mut server,
                    &mut sink,
                    &mut shell,
                    &mut compositor,
                    &mut windows,
                    &mut picker,
                    &mut apps.service,
                    &mut menu,
                );
            }
            // Fold this wake's delivery evidence into the capsule and the
            // monitor's seat view exactly once: the sink latched whether
            // any window owner crossed into or out of the unresponsive set
            // while events were delivered, so both move on a real change
            // and neither is recomputed on a quiet wake.
            deliver_abandoned(
                &mut server,
                &mut sink,
                &mut shell,
                &mut compositor,
                &mut windows,
                &mut picker,
                &mut apps.service,
                &mut menu,
            );
            let vigil_changed = sink.take_changed();
            let mut unresponsive = 0;
            let mut owners = [0u64; SEAT_REPORT_OWNERS_MAX];
            let mut named = 0;
            if vigil_changed {
                unresponsive = sink.unresponsive_count();
                shell.set_tray_unresponsive(&mut compositor, unresponsive);
                named = seat_report_owners(&sink, &server, &windows, &identity, &mut owners);
            }
            maybe_send_seat_report(
                vigil_changed,
                switchboard_pid,
                unresponsive,
                &owners[..named],
                &mut RtSwitchboardMailbox,
            );
            // A launch recorded this wake has a window coming — a spawn, a
            // load, and the app's own bring-up away. Starting its icon now has
            // the picture ready by the time there is a window to put it on,
            // rather than the window opening on the shared application glyph
            // and swapping it a decode later.
            shell.warm_launched_artwork(&compositor, launched.bundles());
            // Bring the application strip up to date before presenting: a
            // declaration that arrived or was withdrawn latches the service
            // dirty, and otherwise the cheap live-window comparison decides
            // — so the strip is re-resolved exactly when an application
            // joined the bar, left it, re-declared, or opened or closed a
            // window.
            // A fresh bundle index is the third reason: an identity the last
            // strip could not resolve is knowable now, and no window or
            // declaration changed to say so. Both latches are drained before
            // the decision, so neither survives a wake the other caused and
            // provokes a second re-resolution on the next one.
            let declared = apps.service.take_dirty();
            let attributed = programs.take_adopted();
            if declared || attributed || app_strip_is_stale(&apps, &shell, &server, &windows) {
                refresh_app_strip(
                    &mut apps,
                    &mut shell,
                    &mut compositor,
                    &server,
                    &windows,
                    &identity,
                    &programs.bundles,
                );
                owner_bundles.publish(switchboard_pid, &apps.strip, &mut RtSwitchboardMailbox);
                // A menu of an application that has left the bar, and its
                // information sheet with it, comes down: nothing is left to
                // answer it.
                let orphaned = matches!(
                    menu.owner(),
                    Some(ChainOwner::Bar(MenuSubject::App { owner, .. }))
                        if !apps.strip.iter().any(|group| group.owner == *owner)
                );
                if orphaned && menu.dismiss() {
                    present_menu_chain(&mut menu, &mut shell, &mut compositor, &windows);
                }
            }
            // Nothing an application opened or raised this wake may surface
            // over a locked screen: the lock goes back on top, beneath only
            // the screensaver, before the frame is shown.
            saver.keep_topmost(&mut compositor);
            lock.keep_topmost(&shell, &mut compositor, saver.window());
            elevate.keep_topmost(&mut compositor, saver.window());
            let text_landed = tairix_font::text_generation() != text_drawn;
            if text_landed {
                text_drawn = tairix_font::text_generation();
                refit_text(
                    &mut shell,
                    &mut compositor,
                    &desktop,
                    (&mut menu, &windows),
                    &mut lock,
                );
            }
            // A prompt keeps the pixels it was painted in, so one standing
            // through a change of look is repainted in the look now in force
            // rather than left in the one the user just left, and one laid out
            // with text that has since landed is laid out again.
            if shell.style_generation() != prompts_style || text_landed {
                prompts_style = shell.style_generation();
                confirm.repaint(&mut shell, &mut compositor);
                elevate.repaint(&shell, &mut compositor);
                resolution.repaint(&mut shell, &mut compositor);
                windows.alerts.repaint(&shell, &mut compositor);
                picker.restyle(&mut shell, &mut compositor);
            }
            // One window thumbnail per wake, so a hover picker over a
            // screenful of windows fills in across the turns the loop was
            // making anyway instead of scaling every frame in one of them.
            shell.advance_window_thumbnails(&mut compositor);
            // Whatever is animating steps to the instant this frame is
            // actually shown at, not to when the wake arrived, so the work
            // this wake did does not age the frame. One clock reading serves
            // the whole frame, as on the deadline path above: the animation
            // and the frame report's rate limit share it.
            let now_ns = tairix_rt::clock_get();
            // An alert or a dialog's content first shows in this frame, so
            // its arm delay runs from now.
            windows.alerts.stamp(now_ns);
            windows.stamp_dialogs(now_ns);
            picker.stamp(now_ns);
            serve_rest(&mut shell, &mut compositor, &mut menu, &windows, now_ns);
            // A lone tap of Ctrl typed this wake shows where the pointer is and
            // one of Meta opens or closes the program library, unless a button
            // or the wheel went with it: Ctrl-click and Ctrl-wheel are gestures
            // of their own. Meta passes over nothing a prompt or a menu holds
            // the seat for.
            if let Some(tap) = keyboard.take_tap() {
                if pointer.quiet_since(tap.pressed_ns) && !switch.is_background() {
                    match tap.key {
                        TapKey::Ctrl => shell.locate_pointer(now_ns),
                        TapKey::Meta => {
                            let modal = tairix_desktop_session::seat_modal(
                                &lock,
                                (&confirm, &elevate, &resolution),
                            );
                            if !modal
                                && !menu.is_open()
                                && shell.toggle_library(&mut compositor, now_ns)
                                    == TaskbarResponse::OpenLibrary
                            {
                                library_opened(
                                    &memory.launcher,
                                    &catalogs,
                                    (&mut shell, &mut compositor),
                                    &mut programs,
                                );
                            }
                        }
                    }
                }
            }
            launcher_turn(
                &mut memory.launcher,
                &mut launched,
                &mut apps,
                (&mut shell, &mut compositor),
            );
            arrangement_turn(
                &mut memory.arrangement,
                &mut desktop,
                &mut windows,
                (&mut shell, &mut compositor),
            );
            pump_private(&mut effects, &mut memory, &private_worker, now_ns);
            animate(
                &mut fade,
                (&mut lock, &mut elevate),
                &mut shell,
                &desktop,
                (&mut compositor, &pacer),
                now_ns,
            );
            // The desktop layer surface's two feeds, resolved once a frame so
            // a burst of pointer samples or window movement costs one message
            // of each rather than one per sample. Ahead of the present, so a
            // surface hidden by a trusted prompt is hidden in the frame that
            // prompt appears in.
            serve_layer_feeds(
                &shell,
                &mut windows,
                &mut compositor,
                &mut server,
                &mut sink,
                trusted_up(&lock, &picker, (&confirm, &elevate, &resolution)),
            );
            // One present per frame deadline: the compositor accumulates the
            // damage the pumped events and served presents produced, and the
            // ring copies only that region once the pacer admits the frame.
            // A frame the lock does not cover is never presented.
            if presentable(&saver, &lock, &compositor)
                && pacer.admit(
                    now_ns,
                    compositor.has_damage() || server.has_pending_frames(),
                    display.as_ref().is_none_or(Presenter::ready),
                    compositor.refresh_rate(),
                )
            {
                let held = seat_held(
                    &lock,
                    (&picker, shell.router().focused()),
                    (&confirm, &elevate, &resolution),
                );
                latch_frames(&mut server, &mut shell, &mut compositor, &mut windows, held);
                answer_presents(&mut server);
                // A window first shown may have taken the keyboard.
                reconcile_focus(
                    &mut focused,
                    &mut server,
                    &mut sink,
                    &mut shell,
                    &mut compositor,
                    &mut windows,
                    &mut picker,
                    &mut apps.service,
                    &mut menu,
                );
                match present(
                    &mut shell,
                    &mut compositor,
                    &mut display,
                    &mut fade,
                    &mut windows,
                    &mut menu,
                    &mut picker,
                    (&mut apps.service, &mut desktop),
                    &mut suspended,
                    TextDrawn {
                        fonts: &font_worker,
                        generation: text_drawn,
                    },
                ) {
                    Ok(true) => saver.shown(&mut compositor),
                    Ok(false) => {}
                    Err(code) => return code,
                }
                pacer.presented(display.as_ref().is_some_and(|ring| !ring.ready()));
            }
            advance_saver(
                (&mut saver, &lock),
                (&mut shell, &mut compositor, &mut pacer),
                (
                    display.as_ref().is_none_or(Presenter::ready),
                    server.has_pending_frames(),
                ),
                now_ns,
            );
            // What that frame cost, for the monitor's Resources page. After
            // the present, so the counts describe pixels already on screen,
            // and silent unless they moved, unless the rate limiter is still
            // holding the last change back, or unless the only content was
            // the Switchboard painting the number itself.
            frames.maybe_send(
                &compositor,
                switchboard_pid,
                frame_content(&mut windows, &server, &identity, switchboard_pid),
                now_ns,
                &mut RtSwitchboardMailbox,
            );
            // And the same counts to the System Information API, where a
            // reader outside this process — a monitor, a shell, a regression
            // gate — asks for them instead of being pushed them.
            frame_stats.maybe_publish(&compositor, now_ns, &mut frame_sink);
            // Misses are recorded as text is laid out and as the frame
            // renders titles, so the fetch is asked for once both are done.
            fetch_drawn_text(&font_worker);
            // The wake is fully handled and its frame is on screen: report
            // what the desktop's caches hold now, before parking again. A
            // change made this turn would otherwise wait for the next wake,
            // which on an idle desktop may be a very long time. Silent
            // unless a figure actually moved.
            tairix_rt::cachereport::publish_if_due();
        }
    }

    /// The session's icon-bar state: the declaration-holding service plus
    /// the strip it last resolved, kept beside the loop so a click resolves
    /// against exactly what the bar shows.
    ///
    /// The slots' icons are not here: the shell owns the one artwork cache
    /// and the seams it reads and decodes through, so the strip's icons and
    /// the rest of the desktop's cannot be cached twice.
    struct AppBarPanel {
        service: AppBarService,
        strip: alloc::vec::Vec<tairix_desktop_session::AppGroup>,
        /// Whether the strip's bundles changed since the launcher was last told
        /// what runs.
        moved: bool,
    }

    /// What the installed programs let the desktop resolve: the file types
    /// each opens, and the bundle directory each kernel-attested application
    /// identity names.
    ///
    /// One value rather than two because one scan produces both, so a click
    /// can never resolve a bundle against a scan the identities did not come
    /// from.
    struct Programs {
        associations: alloc::vec::Vec<AppAssociation>,
        bundles: BundleIndex,
        /// Set when a scan replaced the index. The app strip drains it, so a
        /// slot that took the neutral label while the first scan was still
        /// running adopts its real identity the moment the index lands —
        /// requested, never waited for.
        adopted: bool,
    }

    impl Programs {
        fn new() -> Self {
            Self {
                associations: alloc::vec::Vec::new(),
                bundles: BundleIndex::new(),
                adopted: false,
            }
        }

        /// Take the adoption latch.
        fn take_adopted(&mut self) -> bool {
            core::mem::take(&mut self.adopted)
        }

        /// What the installed bundle at `bundle` declares, when it is known.
        fn association(&self, bundle: &str) -> Option<&AppAssociation> {
            self.associations
                .iter()
                .find(|association| association.bundle_path() == bundle)
        }
    }

    /// What each application does with the documents it is handed, by the
    /// signed manifest of the bundle the kernel attests it runs.
    struct Openers<'a> {
        identity: &'a RtWindowIdentity,
        programs: &'a Programs,
    }

    impl Openers<'_> {
        /// Whether `owner` edits documents. An owner nothing attests, or whose
        /// bundle is not installed, is handed documents to read.
        fn edits(&self, owner: ProcId) -> bool {
            self.association(owner)
                .is_some_and(AppAssociation::writes_documents)
        }

        /// The content types `owner`'s manifest says it opens: none for an
        /// owner nothing attests, so a folder delegates nothing to it.
        fn kinds(&self, owner: ProcId) -> Vec<alloc::string::String> {
            self.association(owner)
                .map(|association| association.mime_types().to_vec())
                .unwrap_or_default()
        }

        fn association(&self, owner: ProcId) -> Option<&AppAssociation> {
            self.identity
                .app_of(owner)
                .and_then(|app| self.programs.bundles.path_of(&app))
                .and_then(|bundle| self.programs.association(bundle))
        }
    }

    impl AppBarPanel {
        fn new() -> Self {
            Self {
                service: AppBarService::new(),
                strip: alloc::vec::Vec::new(),
                moved: false,
            }
        }

        /// Take the latch on the strip's bundles.
        fn take_moved(&mut self) -> bool {
            core::mem::take(&mut self.moved)
        }
    }

    /// Which of the session's directory-listing consumers a request belongs
    /// to: each its own slot and watch, so a pick navigating fast never
    /// displaces the icon column's pending re-list, nor another pick's.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
    enum ListingClient {
        /// The desktop's own icon column — the user's `Desktop` folder.
        Pinboard,
        /// A pick, by the id it was minted with.
        Picker(PickId),
    }

    /// The desktop's wallpaper, slideshow and choosers' previews, prepared on
    /// preparer threads each owning its **own** sandbox worker.
    ///
    /// The icon rasteriser keeps the shared sandbox handle on the session's own
    /// task, untouched; each preparer creates a capability-empty worker inside
    /// itself, so no sandbox handle ever crosses a thread boundary. The policy
    /// is the host-tested [`WallpaperDesk`].
    struct Wallpapers {
        desk: tairix_rt::sync::Mutex<WallpaperDesk>,
        work: tairix_rt::sync::Condvar,
        /// Signalled as a preparer ends a read while an eject holds a volume:
        /// apart from `work`, so a wake meant for a preparer is never taken by
        /// the eject.
        freed: tairix_rt::sync::Condvar,
        wake: alloc::sync::Arc<tairix_rt::sync::WorkerWake>,
    }

    /// A volume's pictures held back from the preparers while it is ejected.
    struct WallpaperHold<'a> {
        wallpapers: &'a Wallpapers,
        hold: u64,
    }

    impl Drop for WallpaperHold<'_> {
        fn drop(&mut self) {
            if self.wallpapers.desk.lock().release(self.hold) {
                self.wallpapers.work.notify_one();
            }
        }
    }

    impl Wallpapers {
        fn new(wake: alloc::sync::Arc<tairix_rt::sync::WorkerWake>) -> Self {
            Self {
                desk: tairix_rt::sync::Mutex::new(WallpaperDesk::new()),
                work: tairix_rt::sync::Condvar::new(),
                freed: tairix_rt::sync::Condvar::new(),
                wake,
            }
        }

        /// Hold every backdrop and slide read from within `roots`, a volume
        /// about to be ejected, back from the preparers until the hold drops,
        /// once the read under way there has ended. `None` where the heap has
        /// no room to note it.
        fn hold_within(&self, roots: &[&[alloc::string::String]]) -> Option<WallpaperHold<'_>> {
            let mut desk = self.desk.lock();
            let hold = desk.hold_within(roots)?;
            while desk.reads_within(roots) {
                desk = self.freed.wait(desk);
            }
            Some(WallpaperHold {
                wallpapers: self,
                hold,
            })
        }

        /// Record what a preparer read, waking an eject waiting it out.
        fn delivered(&self, deliver: impl FnOnce(&mut WallpaperDesk) -> bool) -> bool {
            let mut desk = self.desk.lock();
            let kept = deliver(&mut desk);
            if desk.holds() {
                self.freed.notify_all();
            }
            kept
        }

        /// One preparer's whole life: park until a picture is wanted, read it,
        /// decode it in this thread's own sandbox, and deliver the surface.
        ///
        /// The sandbox seam is built here, once, and owned by this thread; its
        /// worker process starts with the first job and is let go whenever
        /// memory is short and nothing waits.
        fn serve(&self) {
            let mut sandbox = ParserSandbox::new(
                RtLauncher::own_binary(tairix_sandbox::WorkerMemory::Documents),
                tairix_rt::LogSink,
            );
            loop {
                let job = {
                    let mut desk = self.desk.lock();
                    loop {
                        if desk.stopping() {
                            return;
                        }
                        if let Some(job) = desk.next_job() {
                            // The budget may admit more than this one: pass the
                            // wake on rather than leave it to the next arrival.
                            if desk.has_work() {
                                self.work.notify_one();
                            }
                            break Some(job);
                        }
                        if desk.lean() && sandbox.is_live() {
                            break None;
                        }
                        desk = self.work.wait(desk);
                    }
                };
                // Memory is short and nothing waits: the worker goes, off
                // the lock, and the next job starts a fresh one.
                let Some(job) = job else {
                    sandbox.release();
                    continue;
                };
                // The read and the sandbox round trip, with no lock held: these
                // are the calls that used to stall the desktop.
                let kept = match job {
                    WallpaperJob::Backdrop(source) => {
                        let outcome = prepare_wallpaper_surface(&mut sandbox, &source, |planned| {
                            if self.desk.lock().plan_backdrop(planned) {
                                self.work.notify_one();
                            }
                        });
                        self.delivered(|desk| desk.deliver(source, outcome))
                    }
                    WallpaperJob::Slide(source) => {
                        let outcome = prepare_wallpaper_surface(&mut sandbox, &source, |planned| {
                            if self.desk.lock().plan_slide(&source, planned) {
                                self.work.notify_one();
                            }
                        });
                        self.delivered(|desk| desk.deliver_slide(&source, outcome))
                    }
                    WallpaperJob::Preview(mut job) => {
                        let request = job.request;
                        let run = render_wallpaper_preview(&mut sandbox, &mut job, |planned| {
                            self.desk.lock().acquire_preview(&request, planned)
                        });
                        let (outcome, unanswered) = self.desk.lock().after_preview(job, run);
                        // Drawn and its region let go before the desk hears,
                        // with no lock held.
                        if let Some(job) = unanswered {
                            let done = land_preview(job, outcome);
                            self.desk.lock().deliver_preview(done)
                        } else {
                            self.work.notify_one();
                            false
                        }
                    }
                };
                if kept {
                    self.wake.nudge();
                }
            }
        }

        /// Record `job` as a preview to render and wake a preparer.
        ///
        /// # Errors
        ///
        /// The desk's refusal ([`WallpaperDesk::want_preview`]).
        fn want_preview(&self, job: PreviewJob) -> Result<(), Errno> {
            self.desk.lock().want_preview(job)?;
            self.work.notify_one();
            Ok(())
        }

        /// Render previews within what a machine of `memory_total` bytes in
        /// `band` can spare, on at most `preparers` threads.
        fn adopt_band(
            &self,
            preparers: usize,
            memory_total: u64,
            band: tairix_reclaim::PressureBand,
        ) {
            let budget = PreviewBudget::of_machine(
                memory_total,
                band,
                preparers,
                WALLPAPER_PREPARATION_BYTES,
            );
            if self.desk.lock().set_budget(budget) {
                self.work.notify_all();
            }
        }

        /// Take the rendered preview waiting to be handed over, if any.
        fn take_preview(&self) -> Option<PreviewDone> {
            self.desk.lock().take_preview()
        }

        /// Withdraw what closed `window_id` has waiting ([`WallpaperDesk::forget_window`]),
        /// letting the regions it granted go once the desk is released.
        fn forget_window(&self, window_id: u64) {
            let withdrawn = self.desk.lock().forget_window(window_id);
            drop(withdrawn);
        }

        /// Whether a preview of `request` would be accepted now ([`WallpaperDesk::admits`]).
        fn admits(&self, request: &PreviewRequest) -> Result<(), Errno> {
            self.desk.lock().admits(request)
        }

        /// Ask for a slide a saver wants, answering whether it was taken. A
        /// desk with no preparer takes none, rather than decoding on the serve
        /// loop, and the saver is told the slide is unavailable.
        fn want_slide(&self, want: SlideWant, source: WallpaperSource) -> bool {
            let mut desk = self.desk.lock();
            let taken = desk.want_slide(want, source);
            if desk.has_work() {
                self.work.notify_one();
            }
            taken
        }

        fn take_slide(&self) -> Option<(SlideWant, Result<Surface, alloc::string::String>)> {
            self.desk.lock().take_slide()
        }

        fn forget_slides(&self) {
            self.desk.lock().forget_slides();
        }

        /// Record `source` as wanted and wake a preparer.
        ///
        /// With no preparer to answer it the picture is prepared on the calling
        /// thread instead, exactly as the session did before it had one: a
        /// recorded request nobody will serve would leave the backdrop bare
        /// forever.
        fn request(&self, source: &WallpaperSource, own: &SharedSandbox) -> Prepared {
            let deferred = {
                let mut desk = self.desk.lock();
                if desk.stopping() {
                    None
                } else {
                    Some(desk.take(source))
                }
            };
            let Some(prepared) = deferred else {
                if source.image_path().is_none() {
                    return Prepared::Ready {
                        surface: None,
                        refusal: None,
                    };
                }
                return match prepare_wallpaper_surface(&mut own.borrow_mut(), source, |_| {}) {
                    Ok(surface) => Prepared::Ready {
                        surface: Some(surface),
                        refusal: None,
                    },
                    Err(refusal) => Prepared::Ready {
                        surface: None,
                        refusal: Some(refusal),
                    },
                };
            };
            if matches!(prepared, Prepared::Pending) {
                self.work.notify_one();
            }
            prepared
        }

        /// Ask every preparer to leave.
        fn stop(&self) {
            self.desk.lock().stop();
            self.work.notify_all();
        }
    }

    /// Read the image `source` names, place it over its screen in `sandbox`, and
    /// rebuild the result as the surface the compositor blits.
    ///
    /// Every refusal — a file that cannot be read, one larger than any wallpaper,
    /// a malformed image, a crashed worker, or a reply whose pixels do not fill
    /// the screen — *answers* the reason rather than writing it, because this runs
    /// on a worker thread and `stderr` is one descriptor a formatted line reaches
    /// in several writes. The session states it, once, on its own thread; the
    /// desktop falls back to the backdrop colour instead of failing over a
    /// picture.
    fn prepare_wallpaper_surface<L: tairix_sandbox::Launcher, S: tairix_log::Sink>(
        sandbox: &mut ParserSandbox<L, S>,
        source: &WallpaperSource,
        planned: impl FnOnce(u64),
    ) -> Result<Surface, alloc::string::String> {
        let Some(path) = source.image_path() else {
            return Err(alloc::string::String::from(
                "no wallpaper image to prepare; using the backdrop colour",
            ));
        };
        let screen = (source.width, source.height);
        let mut placed = tairix_util::fallible::filled(rgba_len(screen), 0u8).ok_or_else(|| {
            alloc::format!("no memory to draw wallpaper {path}; using the backdrop colour")
        })?;
        let geometry = RenderGeometry {
            screen,
            dest: screen,
            fit: source.fit,
        };
        render_file_into(
            sandbox,
            path,
            MAX_WALLPAPER_BYTES,
            geometry,
            &mut placed,
            |cost| {
                planned(cost.saturating_add(source.surface_bytes()));
                true
            },
        )
        .map_err(|failure| {
            alloc::format!("wallpaper {path} {failure}; using the backdrop colour")
        })?;
        Surface::from_rgba8(source.width, source.height, &placed).ok_or_else(|| {
            alloc::format!("wallpaper {path} did not fill the screen; using the backdrop colour")
        })
    }

    /// The session's shipped-wallpaper service: the catalog it listed at
    /// bring-up and the desk its preparers render through.
    ///
    /// A client's region is mapped once its request is admitted and before it
    /// is queued, so a client that granted something unusable learns so from
    /// its own call rather than from a conclusion that never comes; the job
    /// then carries the mapping to the preparer that draws into it.
    struct Gallery<'a> {
        catalog: &'a [WallpaperName],
        /// The screensavers whose previews a chooser may ask for.
        savers: &'a SaverCatalog,
        desk: &'a Wallpapers,
    }

    /// A client region a preview is drawn into, mapped for the one render.
    struct ClientPixels(tairix_rt::shm::MappedGrant);

    impl PreviewTarget for ClientPixels {
        fn bytes_mut(&mut self) -> &mut [u8] {
            self.0.bytes_mut()
        }
    }

    impl WallpaperService for Gallery<'_> {
        fn catalog(&self) -> &[WallpaperName] {
            self.catalog
        }

        fn render(
            &mut self,
            window_id: u64,
            region: ClientRegion,
            size: tairix_window::PreviewSize,
        ) -> Result<(), Errno> {
            let (path, bound) =
                preview_source(size.subject, self.catalog, self.savers).ok_or(Errno::NotFound)?;
            let request = PreviewRequest {
                window_id,
                client: region.grantor,
                size,
            };
            let least = request.pixel_bytes().ok_or(Errno::LengthOutOfRange)?;
            self.desk.admits(&request)?;
            let mapped = tairix_rt::shm::MappedGrant::map(region.grantor, region.handle, least)?;
            // A refusal drops the job, and with it the mapping, so nothing is
            // held and no conclusion is owed.
            self.desk.want_preview(PreviewJob {
                request,
                path,
                bound,
                target: alloc::boxed::Box::new(ClientPixels(mapped)),
            })
        }
    }

    /// The gallery a teardown bridge carries: it serves no request, so it
    /// offers no catalog and accepts no render.
    ///
    /// A bridge built only to tear a dead client's windows down never
    /// reaches either, exactly as it never serves an `OpenMenu` and says
    /// so by refusing to vouch for the seat.
    struct NoGallery;

    impl WallpaperService for NoGallery {
        fn catalog(&self) -> &[WallpaperName] {
            &[]
        }

        fn render(
            &mut self,
            _window: u64,
            _region: ClientRegion,
            _size: tairix_window::PreviewSize,
        ) -> Result<(), Errno> {
            Err(Errno::NotSupported)
        }
    }

    /// How a chooser's picture is placed *as a screen*.
    ///
    /// A preview shows the picture, not a scale model of the desktop: the
    /// modelled screen is the preview itself, so the placement fills it and
    /// centre-crops whatever does not fit. How the picture will actually be
    /// placed on the real screen is the Fit row's business, which says so in
    /// words rather than in a thumbnail that could not show it honestly.
    const PREVIEW_FIT: tairix_wallpaper::WallpaperFit = tairix_wallpaper::WallpaperFit::Fill;

    /// Read the shipped picture `job` names and render it at the size it asks
    /// for, for one chooser.
    ///
    /// A refusal concludes the preview unrendered: the asking window draws its
    /// placeholder rather than waiting for pixels that are not coming, and
    /// nothing about the store is disclosed beyond the catalog the session
    /// already answered. The reason is not written here — this runs on a
    /// worker thread, where a formatted line would interleave with the
    /// session's own `stderr`.
    fn render_wallpaper_preview<L: tairix_sandbox::Launcher, S: tairix_log::Sink>(
        sandbox: &mut ParserSandbox<L, S>,
        job: &mut PreviewJob,
        acquire: impl FnOnce(u64) -> Acquisition,
    ) -> PreviewRun {
        let size = (
            u32::from(job.request.size.width),
            u32::from(job.request.size.height),
        );
        let Some(out) = job.request.canvas(&mut *job.target) else {
            return PreviewRun::Concluded(PreviewOutcome::Refused);
        };
        let geometry = RenderGeometry {
            screen: size,
            dest: size,
            fit: PREVIEW_FIT,
        };
        let mut asked = None;
        let rendered = render_file_into(sandbox, &job.path, job.bound, geometry, out, |planned| {
            let answer = acquire(planned);
            asked = Some((answer, planned));
            answer == Acquisition::Granted
        });
        match (rendered, asked) {
            (Ok(()), _) => PreviewRun::Concluded(PreviewOutcome::Rendered),
            (Err(FileRenderFailure::Withheld), Some((Acquisition::Wait, planned))) => {
                PreviewRun::Deferred(planned)
            }
            (Err(FileRenderFailure::Withheld), Some((Acquisition::Unavailable, _))) => {
                PreviewRun::Withheld
            }
            (Err(failure), _) if failure.out_of_memory() => {
                PreviewRun::Concluded(PreviewOutcome::Unavailable)
            }
            (Err(_), _) => PreviewRun::Concluded(PreviewOutcome::Refused),
        }
    }

    /// Why drawing a picture from its file stopped.
    enum FileRenderFailure {
        Unreadable(Errno),
        TooLarge,
        Upload(UploadFailure<Errno>),
        Render(WallpaperRenderFailure),
        /// The memory its plan needs was not given to it.
        Withheld,
    }

    impl FileRenderFailure {
        /// Whether only memory was short, so the same render may succeed later.
        const fn out_of_memory(&self) -> bool {
            match self {
                Self::Upload(failure) => failure.out_of_memory(),
                Self::Render(failure) => failure.out_of_memory(),
                Self::Withheld => true,
                Self::Unreadable(_) | Self::TooLarge => false,
            }
        }
    }

    impl core::fmt::Display for FileRenderFailure {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Unreadable(err) | Self::Upload(UploadFailure::Read(err)) => {
                    write!(f, "could not be read ({err})")
                }
                Self::TooLarge => f.write_str("is larger than any wallpaper the desktop renders"),
                Self::Upload(UploadFailure::Shrank) => f.write_str("shrank while it was read"),
                Self::Upload(UploadFailure::NoMemory) => {
                    f.write_str("found no memory to read into")
                }
                Self::Upload(UploadFailure::Document(err)) => {
                    write!(f, "could not be handed to its decoder ({err})")
                }
                Self::Render(err) => write!(f, "could not be rendered ({err})"),
                Self::Withheld => f.write_str("was not given the memory it needs"),
            }
        }
    }

    /// The bytes a straight-alpha RGBA8 picture of `size` takes.
    fn rgba_len(size: (u32, u32)) -> usize {
        usize::try_from(u64::from(size.0) * u64::from(size.1) * 4).unwrap_or(usize::MAX)
    }

    /// How a picture is drawn: the screen it models, the destination drawn,
    /// and how it is fitted.
    #[derive(Copy, Clone)]
    struct RenderGeometry {
        screen: (u32, u32),
        dest: (u32, u32),
        fit: tairix_wallpaper::WallpaperFit,
    }

    /// Draw the picture at `path`, at most `bound` bytes, as `geometry` says,
    /// straight into `out`.
    ///
    /// The file is streamed to the worker a run at a time rather than held
    /// here. Once the worker has read its header, and before anything is
    /// decoded, `admit` is told what the render will hold and answers whether
    /// it may go on.
    fn render_file_into<L: tairix_sandbox::Launcher, S: tairix_log::Sink>(
        sandbox: &mut ParserSandbox<L, S>,
        path: &str,
        bound: usize,
        geometry: RenderGeometry,
        out: &mut [u8],
        admit: impl FnOnce(u64) -> bool,
    ) -> Result<(), FileRenderFailure> {
        let RenderGeometry { screen, dest, fit } = geometry;
        let file = tairix_rt::open(path.as_bytes())
            .map_err(|ret| FileRenderFailure::Unreadable(Errno::from_syscall(ret)))?;
        let length = file
            .regular_len()
            .map_err(FileRenderFailure::Unreadable)?
            .ok_or(FileRenderFailure::Unreadable(Errno::OutOfRange))?;
        let length = usize::try_from(length)
            .ok()
            .filter(|&length| length <= bound)
            .ok_or(FileRenderFailure::TooLarge)?;
        let fd = file.fd();
        upload_document(sandbox, length, |offset, into| {
            tairix_rt::fs_read(fd, offset, into).map_err(Errno::from_syscall)
        })
        .map_err(FileRenderFailure::Upload)?;
        let plan = plan_wallpaper(sandbox, screen, dest.0, dest.1, fit)
            .map_err(FileRenderFailure::Render)?;
        if !admit(plan.peak_bytes()) {
            return Err(FileRenderFailure::Withheld);
        }
        plan.render_into(out).map_err(FileRenderFailure::Render)
    }

    /// The desktop's icon artwork, read and decoded off the serve loop by
    /// readers that each serve one volume's lane at a time, so a stuck volume
    /// holds back only its own pictures.
    ///
    /// Every icon the taskbar, the launcher popup, and the desktop's column
    /// draw costs a bounded read plus a sandbox round trip the first time it is
    /// asked for at a given pixel side. Run on the serve loop that was a visible
    /// freeze — a launcher opening on thirty applications paid it thirty times
    /// before its first pixel. Here it costs the frame that icon spends on its
    /// built-in glyph instead. Each read falls on the lane of the volume it
    /// reads, by `tairix_browse::lanes`, the one policy the listings and the
    /// file manager's readers route by; as many decodes run at once as the
    /// machine leaves room for, each in its reader's own sandbox worker. A
    /// listed bundle's files are followed through their change waits by a
    /// thread of their own (`tairix_browse::follow`, the file manager's too),
    /// so one a copy was still writing when it was read is read again once
    /// they move. The policy is the host-tested [`ArtworkDesk`], lane and
    /// follow policy; this adds the runtime's futex mutex, the condition
    /// variable the readers park on with nothing to do (never a spin), and the
    /// shared wake pipe the wait-set already watches.
    struct Artworks {
        work: tairix_rt::sync::Mutex<ArtworkWork>,
        /// Signalled when work is recorded, and on teardown.
        signal: LaneSignal,
        wake: alloc::sync::Arc<tairix_rt::sync::WorkerWake>,
        /// What the readers share with the thread the followed bundles' files
        /// wake.
        follower: Follower,
    }

    /// What [`Artworks`] holds under its one lock.
    struct ArtworkWork {
        desk: ArtworkDesk<Lane>,
        /// The namespace's bindings each read is routed to its lane by, and
        /// whether they are owed a fresh read.
        routes: Routes,
        reroute: bool,
        /// The lanes a reader is working on, each by one at a time.
        serving: Serving,
        readers: Crew,
        /// How many decodes may run at once.
        decodes: Decodes,
        /// The listed bundles followed, and the follows let go.
        follows: BundleFollows<tairix_rt::File>,
    }

    /// Why a volume's pictures may wait while every icon reader is held up
    /// elsewhere.
    const NO_SPARE_DECODER: &str =
        "a volume's pictures may wait for an icon reader held up on another";

    /// One unit of an artwork reader's work.
    enum ArtworkRead {
        /// Read the namespace's bindings again.
        Route,
        /// Read and decode one picture.
        Decode(ArtworkJob),
        /// Let a follow's waits go.
        Unfollow(FollowedBundle<tairix_rt::File>),
    }

    impl LaneWork for ArtworkWork {
        type Read = ArtworkRead;

        fn lanes(&mut self) -> (&mut Serving, &mut Crew) {
            (&mut self.serving, &mut self.readers)
        }

        /// The namespace's bindings, then a follow to let go, then every icon
        /// before any picture file's own content, which its class picture
        /// already stands in for.
        fn next_read(&mut self, last: Option<Lane>) -> Option<(Lane, ArtworkRead)> {
            if self.desk.stopping() {
                return None;
            }
            let last = last.filter(|lane| self.serving.is_free(*lane));
            last.and_then(|lane| self.next_on(Some(lane)))
                .or_else(|| self.next_on(None))
        }
    }

    impl ArtworkWork {
        /// The next unit of work on a lane no reader serves — `only`, where
        /// named — and its lane.
        fn next_on(&mut self, only: Option<Lane>) -> Option<(Lane, ArtworkRead)> {
            let Self {
                desk,
                routes,
                reroute,
                serving,
                decodes,
                follows,
                ..
            } = self;
            let open = |lane: Lane| serving.is_free(lane) && only.is_none_or(|only| only == lane);
            if *reroute && open(Lane::Services) {
                *reroute = false;
                return Some((Lane::Services, ArtworkRead::Route));
            }
            if let Some((lane, followed)) = follows.next_ending(routes, open) {
                return Some((lane, ArtworkRead::Unfollow(followed)));
            }
            if !decodes.may_start() {
                return None;
            }
            let route = |job: &ArtworkJob| routes.lane_of_artwork(&job.key);
            let mine = |lane: &Lane| open(*lane);
            let (lane, job) = desk
                .next_job_where(route, mine)
                .or_else(|| desk.next_thumbnail_where(route, mine))?;
            decodes.started();
            Some((lane, ArtworkRead::Decode(job)))
        }

        /// Whether a waiting reader is owed a wake for a decode of `key` just
        /// recorded: no reader serves its lane, so none would come to it, and
        /// one more decode may start.
        fn may_take(&self, key: &ArtworkKey) -> bool {
            self.decodes.may_start() && self.serving.is_free(self.routes.lane_of_artwork(key))
        }
    }

    impl FollowWork for ArtworkWork {
        fn follow_parts(&mut self) -> FollowParts<'_> {
            FollowParts {
                desk: &mut self.desk,
                follows: &mut self.follows,
                decodes: &mut self.decodes,
                routes: &self.routes,
            }
        }
    }

    impl Artworks {
        fn new(
            wake: alloc::sync::Arc<tairix_rt::sync::WorkerWake>,
            keys: tairix_hash::BuildSipHash13,
        ) -> Self {
            Self {
                work: tairix_rt::sync::Mutex::new(ArtworkWork {
                    desk: ArtworkDesk::new(keys),
                    routes: Routes::default(),
                    reroute: true,
                    serving: Serving::new(),
                    readers: Crew::new(),
                    decodes: Decodes::new(),
                    follows: BundleFollows::new(),
                }),
                signal: LaneSignal::new(),
                wake,
                follower: Follower::new(),
            }
        }

        /// Let as many decodes run at once as the machine's `memory_bytes`
        /// leave room for.
        fn admit(&self, memory_bytes: u64) {
            let most = tairix_sandbox::WorkerMemory::Artwork.pooled(memory_bytes);
            self.work.lock().decodes.admit(most);
        }

        /// Start the first reader, counted on its way, then the follower,
        /// answering whether the kernel granted the reader.
        fn start(self: &alloc::sync::Arc<Self>) -> bool {
            self.work.lock().readers.start();
            if !self.start_reader() {
                return false;
            }
            self.start_follower();
            true
        }

        /// Start a reader the crew counted on its way: the first at bring-up,
        /// a spare by a reader that took work while none waited. A refusal is
        /// stated once in a run of them.
        fn start_reader(self: &alloc::sync::Arc<Self>) -> bool {
            let served = alloc::sync::Arc::clone(self);
            let started = tairix_rt::thread::Thread::spawn(move || {
                served.work.lock().readers.arrived();
                served.serve();
            });
            match started {
                Ok(handle) => {
                    handle.detach();
                    true
                }
                Err(err) => {
                    if self.work.lock().readers.refused() {
                        app::report(
                            APP_NAME,
                            format_args!("no icon reader thread ({err:?}); {NO_SPARE_DECODER}"),
                        );
                    }
                    false
                }
            }
        }

        /// Start the thread the followed files' changes wake: without it a
        /// bundle is read again only when its listing moves.
        fn start_follower(self: &alloc::sync::Arc<Self>) {
            let artworks = alloc::sync::Arc::clone(self);
            let started = tairix_rt::thread::Thread::spawn(move || {
                let parts = (&artworks.work, artworks.signal.work());
                let follower = &artworks.follower;
                follower.follow(parts, |reason| app::report(APP_NAME, reason));
            });
            match started {
                Ok(handle) => handle.detach(),
                Err(err) => app::report(APP_NAME, Unfollowed::NoThread(err)),
            }
        }

        /// One reader's whole life: take work on a lane no reader serves, do
        /// it with no lock held, deliver it and wake the session, and leave
        /// once two other readers already wait or the desk stops. Its sandbox
        /// worker is made on its first decode and ends with it, or as soon as
        /// memory is short and it has nothing to decode.
        fn serve(self: &alloc::sync::Arc<Self>) {
            let mut seams = (
                ArtworkFileReader(VfsFileReader),
                OwnedSandbox(ParserSandbox::new(
                    RtLauncher::own_binary(tairix_sandbox::WorkerMemory::Artwork),
                    tairix_rt::LogSink,
                )),
            );
            tairix_browse::lanes::serve(
                &self.work,
                &self.signal,
                || {
                    self.start_reader();
                },
                &mut seams,
                |(reader, rasteriser), lane, read| self.read(lane, read, (reader, rasteriser)),
                |(_, rasteriser)| rasteriser.0.release(),
                || self.wake.nudge(),
            );
        }

        /// Let the readers hold their decode workers only while memory is not
        /// short ([`tairix_browse::lanes::adopt_band`]).
        fn adopt_band(&self) {
            tairix_browse::lanes::adopt_band(&self.work, &self.signal);
        }

        /// Carry `read`, taken on `lane`, out through the reader's own `seams`
        /// — the calls that take as long as the disk takes — answering whether
        /// the session is owed a wake.
        fn read(
            &self,
            lane: Lane,
            read: ArtworkRead,
            seams: (&mut ArtworkFileReader<VfsFileReader>, &mut OwnedSandbox),
        ) -> bool {
            match read {
                ArtworkRead::Route => {
                    let routes = Routes::read();
                    {
                        let mut work = self.work.lock();
                        work.routes = routes;
                        work.desk.reroute();
                    }
                    // Work held behind one lane may now fall on a free one.
                    self.signal.notify_all();
                    false
                }
                ArtworkRead::Unfollow(followed) => {
                    tairix_browse::follow::end(&self.work, followed);
                    false
                }
                ArtworkRead::Decode(job) => {
                    let work = (&self.work, self.signal.work());
                    self.follower.decode(work, (lane, &job), seams)
                }
            }
        }

        /// Answer a paint's miss on `key` at `side`, waking a reader for a
        /// decode this call recorded that one may take now: one recorded
        /// earlier a reader already has, and waking one on every paint would
        /// cost a wake per icon per frame.
        fn resolve(&self, key: &ArtworkKey, side: u32) -> Resolved {
            let (answer, wake) = {
                let mut work = self.work.lock();
                let (answer, recorded) = work.desk.collect(key, side);
                (answer, recorded && work.may_take(key))
            };
            if wake {
                self.signal.notify_one();
            }
            answer
        }

        /// Record `key` at `side` as wanted and wake a reader for it, without
        /// waiting for or collecting an answer.
        ///
        /// This is what a warm-up drives: the surface that will draw the icon is
        /// not painting yet, so there is nothing to answer — only work to start.
        fn want(&self, key: &ArtworkKey, side: u32) {
            let wake = {
                let mut work = self.work.lock();
                work.desk.want(key, side) && work.may_take(key)
            };
            if wake {
                self.signal.notify_one();
            }
        }

        /// The version a listed bundle's picture is keyed by, letting go of the
        /// follows of the bundles its desk forgot to remember this one.
        fn version_of(
            &self,
            dir: &str,
            listed: tairix_abi::fs::DocumentStamp,
            side: u32,
        ) -> BundleVersion {
            let (version, let_go) = {
                let mut work = self.work.lock();
                let ArtworkWork { desk, follows, .. } = &mut *work;
                follows.version_of(desk, dir, listed, side)
            };
            if let_go {
                self.signal.notify_one();
            }
            version
        }

        /// What has been delivered since this was last asked.
        fn take_landed(&self) -> tairix_icon::Landed {
            self.work.lock().desk.take_landed()
        }

        /// Withdraw the thumbnails nothing asked for since the last sweep, and
        /// let go of the follows of the bundles no surface asked about.
        fn sweep(&self) {
            let let_go = {
                let mut work = self.work.lock();
                let ArtworkWork { desk, follows, .. } = &mut *work;
                follows.sweep(desk)
            };
            if let_go {
                self.signal.notify_one();
            }
        }

        /// Note that the cache refused to keep this decode's `bytes`, so
        /// nothing asks for it again until the cache would.
        fn decline(&self, key: &ArtworkKey, side: u32, bytes: usize) {
            self.work.lock().desk.decline(key, side, bytes);
        }

        /// Hold back the decodes of what a drop is writing, and picture again
        /// what it finished, waking the loop to repaint what that names and a
        /// reader for the bundles under it read before.
        fn adopt_landing(&self, landing: &LandingWork) {
            let released = {
                let mut work = self.work.lock();
                let released = landing
                    .release
                    .as_deref()
                    .is_some_and(|tree| work.desk.release_under(tree));
                if let Some(tree) = &landing.hold {
                    work.desk.hold_under(tree);
                }
                released
            };
            if released {
                self.wake.nudge();
                self.signal.notify_one();
            }
        }

        /// The band moved: offer again the refused decodes the cache now
        /// `holds`, waking the loop to repaint what was drawn without them.
        fn retry_declined(&self, holds: impl Fn(usize) -> bool) {
            if self.work.lock().desk.retry_declined(holds) {
                self.wake.nudge();
            }
        }

        /// The namespace moved: its bindings are read again, so each read falls
        /// on the lane of the volume that now answers for it.
        fn reroute(&self) {
            self.work.lock().reroute = true;
            self.signal.notify_one();
        }

        /// Let go of the follows of the bundles shown within `roots`, a volume
        /// whose eject a listing reader is about to carry out holding its
        /// `lane` off these readers, and end each follow let go there on that
        /// reader, so none is still open when the eject is made.
        fn end_within(&self, roots: &[&[alloc::string::String]], lane: Lane) {
            {
                let mut work = self.work.lock();
                let ArtworkWork { desk, follows, .. } = &mut *work;
                let _ = follows.release_within(desk, roots);
            }
            loop {
                let ending = {
                    let mut work = self.work.lock();
                    let ArtworkWork {
                        follows, routes, ..
                    } = &mut *work;
                    follows.next_ending(routes, |at| at == lane)
                };
                let Some((_, followed)) = ending else {
                    return;
                };
                tairix_browse::follow::end(&self.work, followed);
            }
        }

        /// Hold the readers off `lane` until the hold drops, once the decode
        /// under way there ends. `None` where the heap has no room to note it.
        fn hold(&self, lane: Lane) -> Option<Held<'_, ArtworkWork>> {
            tairix_browse::lanes::hold(&self.work, &self.signal, lane)
        }

        /// Ask every reader and the follower to leave.
        fn stop(&self) {
            {
                let mut work = self.work.lock();
                work.desk.stop();
                work.readers.stop();
            }
            self.signal.notify_all();
            self.follower.nudge();
        }
    }

    /// The serve loop's [`ArtworkResolver`]: whatever the decoder has already
    /// produced, and otherwise a recorded decode and the built-in glyph for this
    /// frame.
    ///
    /// Held by the shell behind a boxed trait object, which is why it owns its
    /// handle to the desk rather than borrowing one.
    struct DeferredArtwork(alloc::sync::Arc<Artworks>);

    impl ArtworkResolver for DeferredArtwork {
        fn resolve(&mut self, key: &ArtworkKey, side: u32) -> Resolved {
            self.0.resolve(key, side)
        }

        fn prefetch(&mut self, key: &ArtworkKey, side: u32) {
            self.0.want(key, side);
        }

        fn declined(&mut self, key: &ArtworkKey, side: u32, bytes: usize) {
            self.0.decline(key, side, bytes);
        }

        fn bundle_version(
            &mut self,
            dir: &str,
            listed: tairix_abi::fs::DocumentStamp,
            side: u32,
        ) -> BundleVersion {
            self.0.version_of(dir, listed, side)
        }
    }

    /// The session's reads off its loop, so a slow or contended disk cannot
    /// stall the compositor, the seat drain, or an application blocked in a
    /// window call: the desktop's and each pick's directory listings, the
    /// watches that keep each current, the probes its folder icons are
    /// pictured from, and the filesystem calls the desktop makes for the user.
    ///
    /// Each runs on the lane of the volume it touches, one reader to a lane at
    /// a time, so a hung volume stalls only the picks and the pinboard reads
    /// on it. The policy — who asked for what, which answer is stale, whose
    /// turn it is, which watch a listing installs, which folder cue is owed,
    /// which lane a read falls on — is the host-tested [`ListingDesk`],
    /// [`Watches`], [`Probes`] and `tairix_browse::lanes`; this adds the
    /// runtime's futex mutex for exclusion, a condition variable the readers
    /// park on with nothing to do (never a spin), and the write end of the
    /// pipe whose read end is a wait-set member, so the session learns an
    /// answer landed through the very loop it already parks in.
    struct Listings {
        work: tairix_rt::sync::Mutex<ListingWork>,
        /// Signalled when work is recorded, and on teardown.
        signal: LaneSignal,
        wake: alloc::sync::Arc<tairix_rt::sync::WorkerWake>,
        /// The wait-set the session parks in, which each listing's watch joins
        /// once its listing is taken.
        set: u64,
        /// Whether the desktop's folder was said to be unfollowed since its
        /// watch last joined, so the reason is given once, not at every read.
        pinboard_unfollowed: core::sync::atomic::AtomicBool,
        /// The artwork the desktop and its picks draw, whose follows on a
        /// volume the reader carrying out an eject of it ends first.
        artworks: alloc::sync::Arc<Artworks>,
        /// The pictures the desktop paints, none read from a volume while it
        /// is ejected.
        wallpapers: alloc::sync::Arc<Wallpapers>,
    }

    /// What [`Listings`] holds under its one lock: the readers take from every
    /// desk, so one lock is one ordering rather than several that could
    /// interleave. With no reader, `alone`, the session reads, drains and calls
    /// on its own task — draining through `scratch` — and probes nothing.
    struct ListingWork {
        desk: ListingDesk<ListingClient>,
        watches: Watches<ListingClient, alloc::sync::Arc<WatchedDirectory>>,
        probes: Probes,
        /// The filesystem calls asked for, each answered in turn.
        files: tairix_util::defer::JobQueue<FileJob, FileAnswer>,
        /// The namespace's bindings each read is routed to its lane by, and
        /// whether they are owed a fresh read.
        routes: Routes,
        reroute: bool,
        /// The lanes a reader is working on, each by one at a time.
        serving: Serving,
        /// The readers, and the spares that wait while the others work.
        readers: Crew,
        alone: bool,
        scratch: Vec<u8>,
    }

    /// One unit of a reader's work.
    enum ListingRead {
        File(FileJob),
        List(ListingJob<ListingClient>),
        Route,
        Drain(
            ListingClient,
            Vec<alloc::string::String>,
            alloc::sync::Arc<WatchedDirectory>,
        ),
        Probe(Vec<Vec<alloc::string::String>>),
    }

    impl LaneWork for ListingWork {
        type Read = ListingRead;

        fn lanes(&mut self) -> (&mut Serving, &mut Crew) {
            (&mut self.serving, &mut self.readers)
        }

        /// A call the user is waiting on, then a listing someone is waiting
        /// on, then the namespace's bindings, then a change to a folder on
        /// screen, then the folder cues, which a frame already draws without.
        fn next_read(&mut self, last: Option<Lane>) -> Option<(Lane, ListingRead)> {
            if self.desk.stopping() {
                return None;
            }
            let last = last.filter(|lane| self.serving.is_free(*lane));
            last.and_then(|lane| self.next_on(Some(lane)))
                .or_else(|| self.next_on(None))
        }
    }

    impl ListingWork {
        /// The next unit of work on a lane no reader serves — `only`, where
        /// named — and its lane.
        fn next_on(&mut self, only: Option<Lane>) -> Option<(Lane, ListingRead)> {
            let Self {
                desk,
                watches,
                probes,
                files,
                routes,
                reroute,
                serving,
                ..
            } = self;
            let open = |lane: Lane| serving.is_free(lane) && only.is_none_or(|only| only == lane);
            if let Some(job) = files.next_job_where(|job| open(job.lane(routes))) {
                return Some((job.lane(routes), ListingRead::File(job)));
            }
            if let Some(job) = desk.next_job_where(|target| open(routes.lane_of(target))) {
                return Some((routes.lane_of(job.target()), ListingRead::List(job)));
            }
            if *reroute && open(Lane::Services) {
                *reroute = false;
                return Some((Lane::Services, ListingRead::Route));
            }
            let drain = watches.next_drain_where(|location| open(routes.lane_of(location)));
            if let Some((client, location, dir)) = drain {
                let lane = routes.lane_of(&location);
                return Some((lane, ListingRead::Drain(client, location, dir)));
            }
            probes
                .next_batch_by(|folder| {
                    let lane = routes.lane_of(folder);
                    open(lane).then_some(lane)
                })
                .map(|(lane, batch)| (lane, ListingRead::Probe(batch)))
        }
    }

    /// Event id of the desktop's folder listed with no watch to follow it, so
    /// what other programs change there shows only when it is read again.
    const PINBOARD_UNFOLLOWED: EventId = EventId(20_036);

    /// The exact message [`PINBOARD_UNFOLLOWED`] is emitted with.
    const PINBOARD_UNFOLLOWED_MESSAGE: &str = "desktop folder not followed";

    /// The wait-set token a consumer's watch reports under.
    const fn watch_token(client: ListingClient) -> u64 {
        match client {
            ListingClient::Pinboard => PINBOARD_WATCH_TOKEN,
            ListingClient::Picker(pick) => PICKER_WATCH_BASE.saturating_add(pick.get()),
        }
    }

    /// Builds each pick's listing consumer: a slot and a watch of its own on
    /// the session's one desk.
    struct PickerListings(alloc::sync::Arc<Listings>);

    impl PickSources<AsyncDirectorySource> for PickerListings {
        fn source_for(&mut self, pick: PickId) -> AsyncDirectorySource {
            AsyncDirectorySource {
                listings: alloc::sync::Arc::clone(&self.0),
                client: ListingClient::Picker(pick),
            }
        }
    }

    /// Why a volume's reads may wait while every reader is held up elsewhere.
    const NO_SPARE_READER: &str = "a volume's reads may wait for a reader held up on another";

    impl Listings {
        /// A desk with no worker yet, whose watches join `set`, ending what
        /// `artworks` follows on a volume and holding `wallpapers` off it
        /// before an eject of it.
        fn new(
            wake: alloc::sync::Arc<tairix_rt::sync::WorkerWake>,
            set: u64,
            (artworks, wallpapers): (alloc::sync::Arc<Artworks>, alloc::sync::Arc<Wallpapers>),
        ) -> Self {
            // A queue refused the room for its calls is stopped, so each is
            // carried out where it is asked: slower, never wrong.
            let files =
                tairix_util::defer::JobQueue::with_capacity(FILE_JOBS_MAX).unwrap_or_else(|_| {
                    let mut files = tairix_util::defer::JobQueue::new();
                    drop(files.stop());
                    files
                });
            Self {
                work: tairix_rt::sync::Mutex::new(ListingWork {
                    desk: ListingDesk::new(),
                    watches: Watches::new(),
                    probes: Probes::new(),
                    files,
                    routes: Routes::default(),
                    reroute: true,
                    serving: Serving::new(),
                    readers: Crew::new(),
                    alone: false,
                    scratch: Vec::new(),
                }),
                signal: LaneSignal::new(),
                wake,
                set,
                pinboard_unfollowed: core::sync::atomic::AtomicBool::new(false),
                artworks,
                wallpapers,
            }
        }

        /// Start the first reader, counted on its way, answering whether the
        /// kernel granted it.
        fn start(self: &alloc::sync::Arc<Self>) -> bool {
            self.work.lock().readers.start();
            self.start_reader()
        }

        /// Start a reader the crew counted on its way: the first at bring-up,
        /// a spare by a reader that took work while none waited. A refusal is
        /// stated once in a run of them.
        fn start_reader(self: &alloc::sync::Arc<Self>) -> bool {
            let served = alloc::sync::Arc::clone(self);
            let started = tairix_rt::thread::Thread::spawn(move || {
                served.work.lock().readers.arrived();
                served.serve();
            });
            match started {
                Ok(handle) => {
                    handle.detach();
                    true
                }
                Err(err) => {
                    if self.work.lock().readers.refused() {
                        app::report(
                            APP_NAME,
                            format_args!("no reader thread ({err:?}); {NO_SPARE_READER}"),
                        );
                    }
                    false
                }
            }
        }

        /// One reader's whole life: take work on a lane no reader serves, do
        /// it with no lock held, deliver it and wake the session, and leave
        /// once two other readers already wait or the desk stops.
        ///
        /// A read that nobody wants any more is delivered all the same and
        /// reports itself unwanted, so no wake is owed for it — a user clicking
        /// through directories does not make the session repaint once per
        /// abandoned read.
        fn serve(self: &alloc::sync::Arc<Self>) {
            // Refused, each drain reads its changes a few at a time instead.
            let mut scratch =
                tairix_util::fallible::filled(WATCH_BUFFER_LEN, 0).unwrap_or_default();
            tairix_browse::lanes::serve(
                &self.work,
                &self.signal,
                || {
                    self.start_reader();
                },
                &mut scratch,
                |scratch, _, read| self.read(read, scratch),
                |_| {},
                || self.wake.nudge(),
            );
        }

        /// Carry `read` out — the call that can take as long as the disk
        /// takes — answering whether the session is owed a wake.
        fn read(&self, read: ListingRead, scratch: &mut [u8]) -> bool {
            match read {
                ListingRead::File(job) => {
                    let answer = self.carry_out(job);
                    self.work.lock().files.deliver(answer)
                }
                ListingRead::List(job) => {
                    let (client, target) = (job.client(), job.target().to_vec());
                    // A reload of the folder already watched reads through
                    // that watch, so its pacing carries on.
                    let reuse = self.work.lock().watches.relisting(client, &target);
                    let unfollowed = |err| self.unfollowed(client, err);
                    let (listed, armed) = WatchedDirectory::read(&target, reuse, unfollowed);
                    let (owed, unwanted) = {
                        let mut work = self.work.lock();
                        let owed = work.desk.deliver(job, listed);
                        (owed, work.watches.offer_armed(owed, client, &target, armed))
                    };
                    drop(unwanted);
                    owed
                }
                ListingRead::Route => {
                    let routes = Routes::read();
                    self.work.lock().routes = routes;
                    // Work held behind one lane may now fall on a free one.
                    self.signal.notify_all();
                    false
                }
                ListingRead::Drain(client, location, dir) => {
                    let update = dir.drain(scratch);
                    self.work.lock().watches.deliver(client, &location, update)
                }
                ListingRead::Probe(batch) => {
                    let answers = tairix_browse::vfs::probe_batch(
                        batch,
                        tairix_browse::vfs::probe_directory,
                        &mut tairix_browse::RtEntryReader::by_path(),
                    );
                    self.work.lock().probes.deliver(answers)
                }
            }
        }

        /// The namespace moved: its bindings are read again, so each read falls
        /// on the lane of the volume that now answers for it.
        fn reroute(&self) {
            self.work.lock().reroute = true;
            self.signal.notify_one();
        }

        /// Carry `job` out. An eject first holds the icon readers off its
        /// volume, once the read under way there ends, and lets go of what the
        /// session holds there, so none of it refuses the eject as busy.
        fn carry_out(&self, job: FileJob) -> FileAnswer {
            let Some(volume) = job.ejected() else {
                return job.run();
            };
            let _icons = self.artworks.hold(Lane::of_volume(volume));
            // With no room to name where the volume is shown nothing is let
            // go, and the eject says what still holds it.
            let shown = self.shown_at(volume);
            let roots = shown.as_deref().and_then(|shown| {
                tairix_util::fallible::collected(
                    shown.len(),
                    shown.iter().map(|shown| shown.components.as_slice()),
                )
            });
            let _pictures = roots.as_deref().and_then(|roots| {
                let held = self.wallpapers.hold_within(roots);
                self.release_within(roots, volume);
                held
            });
            job.run()
        }

        /// The places the volume published as `volume_id` is shown at, by the
        /// bindings last read; `None` where the heap has no room for them.
        fn shown_at(
            &self,
            volume_id: [u8; tairix_abi::sysinfo::VOLUME_ID_LEN],
        ) -> Option<Vec<tairix_browse::mount::Projection>> {
            let mut shown = Vec::new();
            for projection in self.work.lock().routes.shown_at(volume_id) {
                if !tairix_util::fallible::push(&mut shown, projection) {
                    return None;
                }
            }
            Some(shown)
        }

        /// Let go of each listing's watch, or offer of one, within `roots`,
        /// where the volume published as `volume_id` is shown, and end the
        /// follows of the bundles shown there, here on the reader about to
        /// carry its eject out.
        fn release_within(
            &self,
            roots: &[&[alloc::string::String]],
            volume_id: [u8; tairix_abi::sysinfo::VOLUME_ID_LEN],
        ) {
            loop {
                let released = {
                    let mut work = self.work.lock();
                    let Some(client) = work.watches.holding_within(roots).next() else {
                        break;
                    };
                    work.watches.release_within(client, roots)
                };
                drop(released);
            }
            self.artworks.end_within(roots, Lane::of_volume(volume_id));
        }

        /// Ask for `job`, answering with what it came to when it was not left
        /// for a reader: carried out here because no reader will take it, or
        /// refused because too many calls are already waiting.
        fn submit(&self, job: FileJob) -> Option<FileAnswer> {
            let refused = {
                let mut work = self.work.lock();
                if work.files.stopping() {
                    drop(work);
                    return Some(self.carry_out(job));
                }
                work.files.submit(job).err()
            };
            if let Some(job) = refused {
                return Some(job.refused(Errno::LimitExceeded));
            }
            self.signal.notify_one();
            None
        }

        /// Take the oldest carried-out call, if one has landed.
        fn collect(&self) -> Option<FileAnswer> {
            self.work.lock().files.collect()
        }

        /// Record `components` as `client`'s request and wake a worker.
        ///
        /// With no worker to answer it — the kernel granted no thread, or the
        /// session is tearing down — the read happens on the calling thread
        /// instead, which is exactly what the session did before it had one. A
        /// recorded request nobody will ever serve would leave the desktop
        /// listing forever, so the degradation is a real read, not a wait.
        ///
        /// A listing taken commits its consumer to the watch armed with it,
        /// which joins the session's wait-set here.
        fn request(
            &self,
            client: ListingClient,
            components: &[alloc::string::String],
        ) -> Result<Listing, Errno> {
            let mut took = Took::default();
            let listing = self.ask(client, components, |work| {
                let listing = work.desk.take(client, components);
                if matches!(listing, Ok(Listing::Ready(_))) {
                    took = work.watches.took(client, components);
                }
                listing
            });
            self.commit(client, took);
            listing
        }

        /// Join the watch `client` now reports on to the session's wait-set,
        /// letting go of what it moved away from; a folder that cannot be
        /// followed says so and is unwatched.
        fn commit(&self, client: ListingClient, took: Took<alloc::sync::Arc<WatchedDirectory>>) {
            let joins = took.join.is_some();
            match took.commit(self.set, watch_token(client)) {
                // Followed again, so a later loss is said again.
                Ok(()) if joins && client == ListingClient::Pinboard => self
                    .pinboard_unfollowed
                    .store(false, core::sync::atomic::Ordering::Relaxed),
                Ok(()) => {}
                Err(err) => {
                    self.unwatch(client);
                    self.unfollowed(client, err);
                }
            }
        }

        /// State that `client`'s folder will not follow its changes, and why:
        /// the desktop's once until its watch joins again, on the system log
        /// too, and the picker's unless no volume records changes there, as
        /// for a namespace root.
        fn unfollowed(&self, client: ListingClient, err: Errno) {
            match client {
                ListingClient::Picker(_) if err == Errno::NotImplemented => {}
                ListingClient::Picker(_) => app::report(
                    APP_NAME,
                    format_args!("a folder the picker shows will not refresh by itself ({err})"),
                ),
                ListingClient::Pinboard => {
                    let said = self
                        .pinboard_unfollowed
                        .swap(true, core::sync::atomic::Ordering::Relaxed);
                    if said {
                        return;
                    }
                    app::report(
                        APP_NAME,
                        format_args!("the desktop folder will not refresh by itself ({err})"),
                    );
                    log(
                        &LOG_SINK,
                        &LogEvent {
                            level: LogLevel::Warn,
                            id: PINBOARD_UNFOLLOWED,
                            message: PINBOARD_UNFOLLOWED_MESSAGE,
                            fields: &[LogField {
                                key: "reason",
                                value: LogFieldValue::SignedInt(i64::from(err.as_i32())),
                            }],
                        },
                    );
                }
            }
        }

        /// Record a fresh listing of `components` for `client` — one no read
        /// already under way may answer — degrading exactly as
        /// [`request`](Self::request) does.
        fn refresh(
            &self,
            client: ListingClient,
            components: &[alloc::string::String],
        ) -> Result<Listing, Errno> {
            self.ask(client, components, |work| {
                work.desk.refresh(client, components);
                // The folder may just have changed, so what was probed of its
                // folders may have too.
                work.probes.invalidate_listing(components);
                Ok(Listing::Pending)
            })
        }

        /// Answer the folder cue for `components`, recording a probe for the
        /// worker if none is held or under way.
        ///
        /// Asked while the session resolves the icons it shows, so it never
        /// reads: an unknown cue is [`Probe::Pending`] and is drawn when its
        /// batch lands. With no worker the desk answers that it does not
        /// probe, and the folder draws plain rather than costing the loop a
        /// directory read.
        fn probe(&self, components: &[alloc::string::String]) -> Result<Probe, Errno> {
            let (answer, recorded) = self.work.lock().probes.ask(components);
            if recorded {
                self.signal.notify_one();
            }
            answer
        }

        /// Whether a probe batch has landed since this was last asked. Asked
        /// before each resolve of the icons' cues.
        fn take_probes_landed(&self) -> bool {
            self.work.lock().probes.take_landed()
        }

        /// Drop the probes the pass just resolved did not ask for, waking the
        /// worker if a batch is left for it.
        fn sweep_probes(&self) {
            if self.work.lock().probes.sweep() {
                self.signal.notify_one();
            }
        }

        /// The folders among `changes` in `dir` changed, so what was probed of
        /// them is stale. Whether any was a folder, which owes the cues a
        /// resolve.
        fn invalidate_probes(
            &self,
            dir: &[alloc::string::String],
            changes: &[tairix_browse::EntryChange],
        ) -> bool {
            self.work.lock().probes.invalidate(dir, changes)
        }

        /// Put `client`'s listing request to the desk through `record`, waking
        /// a worker when it leaves the consumer waiting; with no worker to,
        /// read it on this task.
        fn ask(
            &self,
            client: ListingClient,
            components: &[alloc::string::String],
            record: impl FnOnce(&mut ListingWork) -> Result<Listing, Errno>,
        ) -> Result<Listing, Errno> {
            let deferred = {
                let mut work = self.work.lock();
                if work.desk.stopping() {
                    None
                } else {
                    Some(record(&mut work))
                }
            };
            let Some(listing) = deferred else {
                return self.read_here(client, components);
            };
            if matches!(listing, Ok(Listing::Pending)) {
                self.signal.notify_one();
            }
            listing
        }

        /// Read `components` for `client` on this task: armed and joined as a
        /// worker's read would be when the session has no worker, so its folder
        /// still follows its changes, and read bare once it is tearing down.
        fn read_here(
            &self,
            client: ListingClient,
            components: &[alloc::string::String],
        ) -> Result<Listing, Errno> {
            let unfollowed = |err| self.unfollowed(client, err);
            let read = {
                let mut work = self.work.lock();
                let ListingWork { watches, alone, .. } = &mut *work;
                alone.then(|| {
                    watches.read_here(client, components, |reuse| {
                        WatchedDirectory::read(components, reuse, unfollowed)
                    })
                })
            };
            let Some((listed, took)) = read else {
                return read_directory(components).map(Listing::Ready);
            };
            self.commit(client, took);
            listed.map(Listing::Ready)
        }

        /// `client`'s watch reported a change: drain it off the loop, or here
        /// with no worker to. Whether it was drained here and left something
        /// for the loop to adopt now.
        fn want_drain(&self, client: ListingClient) -> bool {
            let mut work = self.work.lock();
            if !work.watches.want_drain(client) {
                return false;
            }
            if work.alone {
                let ListingWork {
                    watches, scratch, ..
                } = &mut *work;
                return watches.drain_here(|dir| dir.drain(scratch));
            }
            drop(work);
            self.signal.notify_one();
            false
        }

        /// What `client`'s drains produced, handed over once.
        fn take_update(&self, client: ListingClient) -> WatchUpdate {
            self.work.lock().watches.take_update(client)
        }

        /// Whether `client`'s listing of `components` follows that folder.
        fn follows(&self, client: ListingClient, components: &[alloc::string::String]) -> bool {
            self.work.lock().watches.follows(client, components)
        }

        /// `client` reports on nothing any more: its folder went.
        fn unwatch(&self, client: ListingClient) {
            let gone = self.work.lock().watches.unwatch(client);
            drop(gone);
        }

        /// `client` is gone, with whatever it had asked for and the watch it
        /// held.
        fn forget(&self, client: ListingClient) {
            let released = {
                let mut work = self.work.lock();
                work.desk.forget(client);
                work.watches.forget(client)
            };
            drop(released);
        }

        /// Ask the readers to leave and wake every one of them; a call still
        /// waiting has nobody left to answer it.
        fn stop(&self) {
            {
                let mut work = self.work.lock();
                work.desk.stop();
                work.watches.stop();
                work.probes.stop();
                drop(work.files.stop());
                work.readers.stop();
                work.alone = false;
            }
            self.signal.notify_all();
        }

        /// No reader will answer: every listing is read, every watch drained
        /// and every call carried out on the session's own task, and no
        /// folder is probed. Refused its scratch, each drain reads its
        /// changes a few at a time instead.
        fn alone(&self) {
            let mut work = self.work.lock();
            work.desk.stop();
            work.probes.stop();
            drop(work.files.stop());
            work.alone = true;
            work.scratch = tairix_util::fallible::filled(WATCH_BUFFER_LEN, 0).unwrap_or_default();
        }
    }

    /// Read the directory named by root-first `components` under this session's
    /// own identity, through the same validated path spelling and stream decode
    /// the synchronous source uses.
    fn read_directory(components: &[alloc::string::String]) -> Result<Vec<Entry>, Errno> {
        let path = tairix_browse::vfs::absolute_path(components)?;
        let stream = tairix_rt::read_dir_all(path.as_bytes()).map_err(Errno::from_syscall)?;
        tairix_browse::vfs::entries_from_dir_stream(
            &path,
            &stream,
            &mut tairix_browse::RtEntryReader::by_path(),
        )
    }

    /// The desktop's settings publisher: the store round trip that adopting a
    /// pinboard change costs, run on a worker thread.
    ///
    /// Persist-then-adopt is preserved and moved off the loop. The serve loop
    /// *submits* the settings a gesture asked for and adopts nothing; the worker
    /// publishes them and answers with what the store then holds; the loop
    /// adopts that on the next wake. So the adopted state still never diverges
    /// from the published document, and the compositor never stops for a disk.
    ///
    /// The desk coalesces latest-wins, which is what makes a settings surface
    /// safe to drive from a continuous control: any number of settled edits
    /// during one publish cost one further publish, not one each.
    struct Publisher {
        desk: tairix_rt::sync::Mutex<tairix_util::defer::JobDesk<PublishJob, PublishAnswer>>,
        /// Signalled when a publish is submitted, and on teardown.
        work: tairix_rt::sync::Condvar,
        wake: alloc::sync::Arc<tairix_rt::sync::WorkerWake>,
        /// The call of a resolution the user kept, watched until it is
        /// answered: its size stays drawn until the store has spoken.
        kept: tairix_rt::sync::Mutex<KeptCall>,
        /// The newest settings asked to be published, until it lands.
        asked: tairix_rt::sync::Mutex<AskedSettings>,
    }

    /// The kept resolution's pinboard call, and what it was answered once it
    /// has been.
    #[derive(Default)]
    struct KeptCall {
        ticket: Option<u64>,
        answered: Option<Result<(), Errno>>,
    }

    /// A publish the desktop wants, and the pinboard call waiting on it.
    ///
    /// The ticket rides with the settings because the chooser's reply must state
    /// what the store actually did, not merely that its request was decoded — so
    /// the call is answered when the publish lands. A request the user's next
    /// gesture displaces before it is taken is answered right there, so no
    /// caller is ever left parked on an answer nobody will produce.
    struct PublishJob {
        settings: DesktopSettings,
        /// The file manager's settings, published in the same document: a job
        /// always carries both, so the newest job is a whole document.
        files: FilerPreferences,
        /// The `PINBOARD_ENDPOINT` call to answer, for a publish a foreign
        /// application asked for. `None` for one the desktop asked itself.
        ticket: Option<u64>,
        /// Which ask it is, in the order asked.
        number: u64,
        /// Whether it was made over the settings the store holds; one that was
        /// not reads the store in its place.
        over_stored: bool,
    }

    /// What publishing the desktop's settings produced: the settings to adopt,
    /// the refusal to state, and the call to answer with either.
    struct PublishAnswer {
        outcome: Result<Publication, Errno>,
        ticket: Option<u64>,
        number: u64,
    }

    impl PublishAnswer {
        /// What the call that asked for the publish is answered.
        fn reply(&self) -> Result<(), Errno> {
            match &self.outcome {
                Ok(publication) => publication.answer(),
                Err(err) => Err(*err),
            }
        }
    }

    impl Publisher {
        /// A desk with no worker yet.
        fn new(wake: alloc::sync::Arc<tairix_rt::sync::WorkerWake>) -> Self {
            Self {
                desk: tairix_rt::sync::Mutex::new(tairix_util::defer::JobDesk::new()),
                work: tairix_rt::sync::Condvar::new(),
                wake,
                kept: tairix_rt::sync::Mutex::new(KeptCall::default()),
                asked: tairix_rt::sync::Mutex::new(AskedSettings::new()),
            }
        }

        /// The settings the session's own next change is made over: the newest
        /// it has asked to publish, or `adopted` once every ask has landed. So
        /// a change of its own never undoes one still on its way to the store.
        fn base(&self, adopted: &DesktopSettings) -> DesktopSettings {
            self.asked.lock().base(adopted)
        }

        /// The file manager's settings the next change is made over, as
        /// [`base`](Self::base) is for the desktop's.
        fn files_base(&self, adopted: &FilerPreferences) -> FilerPreferences {
            self.asked.lock().files_base(adopted)
        }

        /// Whether a publish asked for has yet to land: the settings in force
        /// are about to change, so nothing is drawn from them meanwhile.
        fn publishing(&self) -> bool {
            self.asked.lock().outstanding()
        }

        /// Note that ask `number` has landed, adopted or refused.
        fn landed(&self, number: u64) {
            self.asked.lock().landed(number);
        }

        /// Note that the settings adopted are the ones the store holds, so a
        /// change made from here on is published.
        fn read_store(&self) {
            self.asked.lock().read_store();
        }

        /// Answer the pinboard call `ticket`, noting it if it is the kept
        /// resolution's.
        ///
        /// Every way a call this publisher took ends — adopted, overtaken, or
        /// displaced — is answered here, so a kept resolution can never wait
        /// on an answer that was given somewhere else.
        fn answer(&self, ticket: u64, outcome: Result<(), Errno>) {
            reply_pinboard(ticket, outcome);
            let mut kept = self.kept.lock();
            if kept.ticket == Some(ticket) {
                kept.answered = Some(outcome);
            }
        }

        /// Watch `ticket`, the call of a resolution the user just kept.
        fn keep(&self, ticket: u64) {
            *self.kept.lock() = KeptCall {
                ticket: Some(ticket),
                answered: None,
            };
        }

        /// Whether a kept resolution's call is still unanswered.
        fn keeping(&self) -> bool {
            let kept = self.kept.lock();
            kept.ticket.is_some() && kept.answered.is_none()
        }

        /// What the kept resolution's call was answered, if it has been since
        /// this was last asked, forgetting it if so.
        fn take_kept_answered(&self) -> Option<Result<(), Errno>> {
            let mut kept = self.kept.lock();
            let answered = kept.answered;
            if answered.is_some() {
                *kept = KeptCall::default();
            }
            answered
        }

        /// One publisher's whole life: park until settings are submitted,
        /// publish them, deliver what the store then holds, wake the session.
        fn serve(&self) {
            loop {
                let job = {
                    let mut desk = self.desk.lock();
                    loop {
                        if desk.stopping() {
                            return;
                        }
                        if let Some(job) = desk.next_job() {
                            break job;
                        }
                        desk = self.work.wait(desk);
                    }
                };
                // The store round trip, with no lock held.
                let answer = PublishAnswer {
                    outcome: publish_asked(&mut RtHost, &job.settings, &job.files, job.over_stored),
                    ticket: job.ticket,
                    number: job.number,
                };
                // A publish the user's next gesture superseded while it ran is
                // dropped for *adoption* — the queued one will replace what it
                // would have shown — but it did happen, so its caller learns
                // the outcome it actually got rather than a refusal.
                let outcome = answer.reply();
                let ticket = answer.ticket;
                if self.desk.lock().deliver(answer) {
                    self.wake.nudge();
                } else if let Some(ticket) = ticket {
                    self.answer(ticket, outcome);
                }
            }
        }

        /// Ask for `settings` and `files` to be published, answering the
        /// pinboard call `ticket` when the store has spoken.
        ///
        /// With no publisher to answer it the publish happens on the calling
        /// thread, exactly as the session did before it had one: a submitted
        /// change nobody will serve would leave the desktop showing settings it
        /// never adopted.
        fn submit(
            &self,
            settings: DesktopSettings,
            files: FilerPreferences,
            ticket: Option<u64>,
        ) -> Option<PublishAnswer> {
            let ask = self.asked.lock().ask(&settings, &files);
            let submitted = {
                let mut desk = self.desk.lock();
                if desk.stopping() {
                    drop(desk);
                    return Some(PublishAnswer {
                        outcome: publish_asked(&mut RtHost, &settings, &files, ask.over_stored),
                        ticket,
                        number: ask.number,
                    });
                }
                desk.submit(PublishJob {
                    settings,
                    files,
                    ticket,
                    number: ask.number,
                    over_stored: ask.over_stored,
                })
            };
            if submitted.wake {
                self.work.notify_one();
            }
            // A gesture that overtook an application's request before any
            // worker took it: that request is not going to be published, and
            // saying so beats leaving its caller parked.
            if let Some(displaced) = submitted.displaced {
                if let Some(ticket) = displaced.ticket {
                    self.answer(ticket, Err(Errno::Busy));
                }
            }
            None
        }

        /// Take a landed publish, if one has.
        fn collect(&self) -> Option<PublishAnswer> {
            self.desk.lock().collect()
        }

        /// Ask the worker to leave and wake it.
        fn stop(&self) {
            self.desk.lock().stop();
            self.work.notify_all();
        }
    }

    /// The program catalogue and the file associations its bundles declare,
    /// read on a worker thread.
    ///
    /// Two configuration documents and then one `AppInfo` per catalogued
    /// application: on a machine with a full program store that is far more
    /// than a frame's worth of reads, and it used to happen on the very click
    /// that opened the launcher. The popup now opens on the catalogue already
    /// in hand and adopts the fresh one when it lands.
    struct Catalogs {
        desk: tairix_rt::sync::Mutex<tairix_util::defer::JobDesk<(), LoadedPrograms>>,
        /// Signalled when a scan is submitted, and on teardown.
        work: tairix_rt::sync::Condvar,
        wake: alloc::sync::Arc<tairix_rt::sync::WorkerWake>,
    }

    impl Catalogs {
        /// A desk with no worker yet.
        fn new(wake: alloc::sync::Arc<tairix_rt::sync::WorkerWake>) -> Self {
            Self {
                desk: tairix_rt::sync::Mutex::new(tairix_util::defer::JobDesk::new()),
                work: tairix_rt::sync::Condvar::new(),
                wake,
            }
        }

        /// One scanner's whole life: park until a scan is wanted, read the
        /// stores and the manifests, deliver the snapshot, wake the session.
        fn serve(&self) {
            loop {
                {
                    let mut desk = self.desk.lock();
                    loop {
                        if desk.stopping() {
                            return;
                        }
                        if desk.next_job().is_some() {
                            break;
                        }
                        desk = self.work.wait(desk);
                    }
                }
                // The reads, with no lock held.
                let loaded = load_programs(&mut VfsFileReader, &mut RtHost, home_dir().as_deref());
                if self.desk.lock().deliver(loaded) {
                    self.wake.nudge();
                }
            }
        }

        /// Ask for a fresh scan, answering with the snapshot when there is no
        /// scanner to do it elsewhere.
        fn submit(&self) -> Option<LoadedPrograms> {
            let submitted = {
                let mut desk = self.desk.lock();
                if desk.stopping() {
                    drop(desk);
                    return Some(load_programs(
                        &mut VfsFileReader,
                        &mut RtHost,
                        home_dir().as_deref(),
                    ));
                }
                desk.submit(())
            };
            if submitted.wake {
                self.work.notify_one();
            }
            None
        }

        /// Take a landed scan, if one has.
        fn collect(&self) -> Option<LoadedPrograms> {
            self.desk.lock().collect()
        }

        /// Ask the worker to leave and wake it.
        fn stop(&self) {
            self.desk.lock().stop();
            self.work.notify_all();
        }
    }

    /// The most filesystem calls the desktop holds for the user at once,
    /// counting those carried out but not yet settled.
    ///
    /// A containment bound on what gestures can queue behind a slow or failing
    /// disk, not a capacity: each is a click, and one past it is refused with
    /// its reason rather than queued without limit.
    const FILE_JOBS_MAX: usize = 16;

    /// A filesystem call the desktop makes for the user, carried out on the
    /// reader of the volume it touches so the compositing loop never waits on
    /// a disk.
    enum FileJob {
        /// Open what pick `pick` chose, for its attempt `serial`, for a
        /// requester that `edits` documents or does not.
        Pick {
            pick: PickId,
            serial: u64,
            path: alloc::string::String,
            access: PickAccess,
            edits: bool,
            /// How a save's file is typed where it is made.
            typing: Typing,
        },
        /// Open the files of the folder pick `pick` chose whose content type
        /// is one of `kinds`, for its attempt `serial`.
        Folder {
            pick: PickId,
            serial: u64,
            path: alloc::string::String,
            kinds: Vec<alloc::string::String>,
        },
        /// Make the folder pick `pick`'s save asked for, for its attempt
        /// `serial`.
        NewFolder {
            pick: PickId,
            serial: u64,
            path: alloc::string::String,
        },
        /// Take back the file a save made after its pick had gone: the name
        /// at `path` is removed only while it still names `file`.
        Discard {
            file: tairix_rt::File,
            path: alloc::string::String,
        },
        /// A call for the user's own gesture on the desktop.
        Desktop(DesktopCall),
    }

    /// A filesystem call a desktop gesture asks for.
    enum DesktopCall {
        /// Open a document the user opened from the desktop, for the bundle
        /// whose entry binary is `run_path`, reported as `label`.
        Document {
            run_path: alloc::string::String,
            label: alloc::string::String,
            document: LaunchDocument,
        },
        /// Open read-only the file carried off the desktop and dropped in a
        /// window, to be placed in that window's document.
        Place(alloc::boxed::Box<PlacedItem>),
        /// Make the folder the user asked for.
        Folder { path: alloc::string::String },
        /// Make the shortcut the user asked for, storing `target`.
        Shortcut {
            link: alloc::string::String,
            target: alloc::string::String,
        },
        /// Read the mount table, which says where each volume the desktop
        /// shows is.
        Mounts,
        /// Eject or retitle a volume a drive the desktop draws stands for.
        Volume(tairix_browse::mount::MountAct),
    }

    /// A file opened for the user, which closes when it is dropped.
    type Opened = Result<tairix_browse::document::Opened, Errno>;

    /// The files of a chosen folder its requester opens, opened in the order
    /// the picker lists them, and how many more were not.
    struct FolderFiles {
        files: Vec<(tairix_browse::document::Opened, alloc::string::String)>,
        left_out: u32,
    }

    /// What a [`FileJob`] came to, in the shape its own kind of call yields,
    /// so an open never answers with nothing opened.
    enum FileAnswer {
        /// What pick `pick` chose, opened for its attempt `serial`, the path
        /// the open made where it made a new file, and the path it is at, a
        /// save's name finished.
        Pick {
            pick: PickId,
            serial: u64,
            opened: Opened,
            created: Option<alloc::string::String>,
            made: alloc::string::String,
        },
        /// The files of the folder pick `pick` chose, opened for its attempt
        /// `serial`.
        Folder {
            pick: PickId,
            serial: u64,
            opened: Result<FolderFiles, Errno>,
        },
        /// The folder pick `pick`'s save asked for at `path`, made for its
        /// attempt `serial`.
        NewFolder {
            pick: PickId,
            serial: u64,
            made: Result<(), Errno>,
            path: alloc::string::String,
        },
        /// A desktop gesture's call.
        Desktop(DesktopAnswer),
        /// A file taken back, or left where it no longer names what was made.
        Discarded,
    }

    /// A syscall's return as a status: negative is the kernel's refusal.
    fn status(ret: i64) -> Result<(), Errno> {
        if ret < 0 {
            Err(Errno::from_syscall(ret))
        } else {
            Ok(())
        }
    }

    impl DesktopCall {
        /// Carry the call out, on the file worker.
        fn run(self) -> DesktopAnswer {
            match self {
                Self::Document {
                    run_path,
                    label,
                    document,
                } => {
                    let opened =
                        tairix_browse::document::open_for(document.path.as_bytes(), document.edits);
                    DesktopAnswer::Document {
                        run_path,
                        label,
                        document,
                        opened,
                    }
                }
                Self::Place(placed) => {
                    let opened = tairix_browse::document::open_for(placed.path.as_bytes(), false);
                    DesktopAnswer::Placed { placed, opened }
                }
                Self::Folder { path } => {
                    let made = status(tairix_rt::fs_mkdir(
                        path.as_bytes(),
                        tairix_abi::fs::FS_NEW_DIR_MODE,
                    ));
                    DesktopAnswer::Made { path, made }
                }
                // Target first, then link: the stored target is data the
                // kernel never resolves here, and a name already taken is the
                // kernel's own refusal — this never replaces one.
                Self::Shortcut { link, target } => {
                    let made = status(tairix_rt::fs_symlink(target.as_bytes(), link.as_bytes()));
                    DesktopAnswer::Made { path: link, made }
                }
                Self::Mounts => {
                    let mut volumes = Vec::new();
                    let read = tairix_procinfo::for_each_mount(&IpcTransport, |record| {
                        volumes.extend(tairix_browse::Volume::from_mount(record));
                        Ok(tairix_procinfo::WalkStep::Continue)
                    });
                    DesktopAnswer::Mounts(read.is_ok().then_some(volumes))
                }
                Self::Volume(act) => DesktopAnswer::Volume(tairix_browse::mount::carry_out(act)),
            }
        }
    }

    /// What a [`DesktopCall`] came to.
    enum DesktopAnswer {
        /// The document opened for the bundle whose entry binary is
        /// `run_path`, reported as `label`.
        Document {
            run_path: alloc::string::String,
            label: alloc::string::String,
            document: LaunchDocument,
            opened: Opened,
        },
        /// The file opened to be placed where `placed` says.
        Placed {
            placed: alloc::boxed::Box<PlacedItem>,
            opened: Opened,
        },
        /// The folder or shortcut made at `path`.
        Made {
            path: alloc::string::String,
            made: Result<(), Errno>,
        },
        /// What a read of the mount table found, or `None` for one that
        /// failed.
        Mounts(Option<Vec<tairix_browse::Volume>>),
        /// A read of the mount table refused before it began.
        MountsRefused,
        /// What an eject or a retitle came to.
        Volume(tairix_browse::mount::MountDone),
    }

    impl FileJob {
        /// The lane of the volume the call touches.
        fn lane(&self, routes: &Routes) -> Lane {
            match self {
                Self::Pick { path, .. }
                | Self::Folder { path, .. }
                | Self::NewFolder { path, .. }
                | Self::Discard { path, .. }
                | Self::Desktop(
                    DesktopCall::Folder { path } | DesktopCall::Shortcut { link: path, .. },
                ) => routes.lane_of_path(path),
                Self::Desktop(DesktopCall::Document { document, .. }) => {
                    routes.lane_of_path(&document.path)
                }
                Self::Desktop(DesktopCall::Place(placed)) => routes.lane_of_path(&placed.path),
                Self::Desktop(DesktopCall::Mounts) => Lane::Services,
                Self::Desktop(DesktopCall::Volume(act)) => routes.lane_of_act(act),
            }
        }

        /// The volume this call ejects, where it is an eject.
        fn ejected(&self) -> Option<[u8; tairix_abi::sysinfo::VOLUME_ID_LEN]> {
            match self {
                Self::Desktop(DesktopCall::Volume(tairix_browse::mount::MountAct::Eject {
                    volume_id,
                    ..
                })) => Some(*volume_id),
                _ => None,
            }
        }

        /// Carry the call out.
        fn run(self) -> FileAnswer {
            match self {
                Self::Pick {
                    pick,
                    serial,
                    path,
                    access,
                    edits,
                    typing,
                } => {
                    let (opened, created, made) = match access.save_flags() {
                        Some(flags) => {
                            open_save(&path, flags, typing, access == PickAccess::Create)
                        }
                        None => (
                            tairix_browse::document::open_for(path.as_bytes(), edits),
                            None,
                            path,
                        ),
                    };
                    FileAnswer::Pick {
                        pick,
                        serial,
                        opened,
                        created,
                        made,
                    }
                }
                Self::Discard { file, path } => {
                    discard(&file, &path);
                    FileAnswer::Discarded
                }
                Self::Folder {
                    pick,
                    serial,
                    path,
                    kinds,
                } => FileAnswer::Folder {
                    pick,
                    serial,
                    opened: open_folder(&path, &kinds),
                },
                Self::NewFolder { pick, serial, path } => FileAnswer::NewFolder {
                    pick,
                    serial,
                    made: status(tairix_rt::fs_mkdir(
                        path.as_bytes(),
                        tairix_abi::fs::FS_NEW_DIR_MODE,
                    )),
                    path,
                },
                Self::Desktop(call) => FileAnswer::Desktop(call.run()),
            }
        }

        /// Answer the call as refused with `err`, without carrying it out.
        fn refused(self, err: Errno) -> FileAnswer {
            match self {
                Self::Pick {
                    pick, serial, path, ..
                } => FileAnswer::Pick {
                    pick,
                    serial,
                    opened: Err(err),
                    created: None,
                    made: path,
                },
                Self::Discard { .. } => FileAnswer::Discarded,
                Self::Folder { pick, serial, .. } => FileAnswer::Folder {
                    pick,
                    serial,
                    opened: Err(err),
                },
                Self::NewFolder { pick, serial, path } => FileAnswer::NewFolder {
                    pick,
                    serial,
                    made: Err(err),
                    path,
                },
                Self::Desktop(DesktopCall::Document {
                    run_path,
                    label,
                    document,
                }) => FileAnswer::Desktop(DesktopAnswer::Document {
                    run_path,
                    label,
                    document,
                    opened: Err(err),
                }),
                Self::Desktop(DesktopCall::Place(placed)) => {
                    FileAnswer::Desktop(DesktopAnswer::Placed {
                        placed,
                        opened: Err(err),
                    })
                }
                Self::Desktop(DesktopCall::Mounts) => {
                    FileAnswer::Desktop(DesktopAnswer::MountsRefused)
                }
                Self::Desktop(DesktopCall::Volume(act)) => {
                    FileAnswer::Desktop(DesktopAnswer::Volume(tairix_browse::mount::MountDone {
                        act,
                        answer: Err(err),
                    }))
                }
                Self::Desktop(
                    DesktopCall::Folder { path } | DesktopCall::Shortcut { link: path, .. },
                ) => FileAnswer::Desktop(DesktopAnswer::Made {
                    path,
                    made: Err(err),
                }),
            }
        }
    }

    /// Remove the name at `path` while it still names the node `file` holds:
    /// a name replaced since by another file is left as it is.
    fn discard(file: &tairix_rt::File, path: &str) {
        let Some((folder, name)) = path.rsplit_once('/') else {
            return;
        };
        let folder = if folder.is_empty() { "/" } else { folder };
        let (Ok(stat), Ok(dir)) = (file.stat(), tairix_rt::Dir::open(folder.as_bytes())) else {
            return;
        };
        let expect = tairix_abi::fs::NodeExpect {
            id: stat.id,
            content_gen: 0,
            size: 0,
            modified: tairix_abi::time::Time64::UNIX_EPOCH,
            changed: tairix_abi::time::Time64::UNIX_EPOCH,
        };
        let _ = dir.unlink(
            name.as_bytes(),
            tairix_abi::fs::UnlinkFlags::EXPECT_NODE,
            Some(&expect),
        );
    }

    /// Why the name at `path` an exclusive create refused is taken:
    /// [`Errno::AlreadyExists`] for a file a save may replace once asked,
    /// [`Errno::IsADirectory`] for a folder and [`Errno::LinkLoop`] for a
    /// link, which no save replaces.
    ///
    /// The node is resolved without following a link and opened for neither
    /// read nor write, so this reads only its kind.
    /// Make, or open to replace, the file a save chose at `path`, with
    /// `flags`: its name finished as `typing` says for the volume its folder
    /// is on, and its file type recorded where that volume records one, before
    /// any requester holds it. Made relative to the folder it asked of, so the
    /// folder cannot change between the asking and the making. Answers what
    /// was opened, the path a new file was made at where one was, and the
    /// path the file is at.
    fn open_save(
        path: &str,
        flags: tairix_abi::fs::OpenFlags,
        typing: Typing,
        exclusive: bool,
    ) -> (Opened, Option<alloc::string::String>, alloc::string::String) {
        let refused = |err: Errno, at: &str| (Err(err), None, alloc::string::String::from(at));
        let Some((folder, asked)) = path.rsplit_once('/') else {
            return refused(Errno::OutOfRange, path);
        };
        let folder = if folder.is_empty() { "/" } else { folder };
        let dir = match tairix_rt::Dir::open(folder.as_bytes()) {
            Ok(dir) => dir,
            Err(ret) => return refused(Errno::from_syscall(ret), path),
        };
        let records = tairix_browse::vfs::keeps_filetypes(&dir);
        let Some(leaf) = typing.leaf(asked, records) else {
            return refused(Errno::LengthOutOfRange, path);
        };
        let made = if folder == "/" {
            alloc::format!("/{leaf}")
        } else {
            alloc::format!("{folder}/{leaf}")
        };
        let file = match dir.create_file(leaf.as_bytes(), flags, tairix_abi::fs::FS_NEW_FILE_MODE) {
            Ok(file) => file,
            Err(ret) => {
                let err = match Errno::from_syscall(ret) {
                    Errno::AlreadyExists => occupant_of(&made),
                    err => err,
                };
                return refused(err, &made);
            }
        };
        if let (Some(filetype), true) = (typing.filetype, records) {
            let value = tairix_fsmeta::preset::acorn::filetype_to_value(filetype);
            let set = tairix_rt::fs_attr_set(
                file.fd(),
                tairix_fsmeta::preset::acorn::FILETYPE_KEY,
                &value,
            );
            if set < 0 {
                // A file of the wrong type is not left behind.
                if exclusive {
                    discard(&file, &made);
                }
                return refused(Errno::from_syscall(set), &made);
            }
        }
        let created = exclusive.then(|| made.clone());
        (
            Ok(tairix_browse::document::Opened {
                file,
                writable: true,
            }),
            created,
            made,
        )
    }

    fn occupant_of(path: &str) -> Errno {
        let kind = tairix_rt::File::open(path.as_bytes(), tairix_abi::fs::OpenFlags::NO_FOLLOW)
            .ok()
            .and_then(|node| node.stat().ok())
            .map(|stat| stat.kind);
        match kind {
            Some(tairix_abi::fs::FileKind::Directory) => Errno::IsADirectory,
            Some(tairix_abi::fs::FileKind::Symlink) => Errno::LinkLoop,
            Some(tairix_abi::fs::FileKind::Regular) | None => Errno::AlreadyExists,
        }
    }

    /// Open, read-only, the files of the folder at `path` the picker's
    /// selection keeps for `kinds`.
    ///
    /// Opened without following a link, so a name turned into one since the
    /// listing hands over nothing. A file that will not open is left out, and
    /// counted.
    fn open_folder(path: &str, kinds: &[alloc::string::String]) -> Result<FolderFiles, Errno> {
        let components = tairix_browse::vfs::components_from_absolute_path(path)?;
        let mut entries = read_directory(&components)?;
        let mut found = FolderFiles {
            files: Vec::new(),
            left_out: tairix_desktop_session::picker::folder_selection(&mut entries, kinds),
        };
        for entry in &entries {
            let mut child = alloc::string::String::from(path);
            tairix_path::push_name(&mut child, entry.name());
            let opened = tairix_rt::File::open(
                child.as_bytes(),
                tairix_abi::fs::OpenFlags::READ.union(tairix_abi::fs::OpenFlags::NO_FOLLOW),
            );
            match opened {
                Ok(file) => found.files.push((
                    tairix_browse::document::Opened {
                        file,
                        writable: false,
                    },
                    alloc::string::String::from(entry.name()),
                )),
                Err(_) => found.left_out = found.left_out.saturating_add(1),
            }
        }
        Ok(found)
    }

    /// One consumer's view of [`Listings`]: a [`DirectorySource`] that records a
    /// request and answers with whatever has come back. Every consumer reaches
    /// the one desk; dropping one — a pick closing — lets go of what it asked
    /// for and the watch it held.
    struct AsyncDirectorySource {
        listings: alloc::sync::Arc<Listings>,
        client: ListingClient,
    }

    impl Drop for AsyncDirectorySource {
        fn drop(&mut self) {
            self.listings.forget(self.client);
        }
    }

    impl DirectorySource for AsyncDirectorySource {
        fn list(&mut self, components: &[alloc::string::String]) -> Result<Listing, Errno> {
            self.listings.request(self.client, components)
        }

        fn refresh(&mut self, components: &[alloc::string::String]) -> Result<Listing, Errno> {
            self.listings.refresh(self.client, components)
        }

        fn follows(&self, components: &[alloc::string::String]) -> bool {
            self.listings.follows(self.client, components)
        }

        fn has_children(&mut self, components: &[alloc::string::String]) -> Result<Probe, Errno> {
            match self.client {
                ListingClient::Pinboard => self.listings.probe(components),
                // Choosing a file gains nothing from a cue, so a pick reads no
                // folder it is not showing.
                ListingClient::Picker(_) => Err(Errno::NotImplemented),
            }
        }
    }

    /// The pool each composite's per-pixel work is spread across: one
    /// participant per `online` CPU, of which the serve loop's own thread is
    /// one.
    ///
    /// A machine that reports one CPU, and a session that is refused a thread,
    /// both compose on the serve loop's own thread and pay nothing for the
    /// machinery: fewer cores is slower, never wrong.
    ///
    /// The pool lives as long as the session does, so it is created once here and
    /// leaked deliberately — its workers are process-lifetime threads, and a
    /// pool torn down at some arbitrary point would only mean joining them again
    /// at exit.
    fn composite_pool(online: usize) -> &'static Pool {
        alloc::boxed::Box::leak(alloc::boxed::Box::new(app::pool_across(
            APP_NAME,
            online,
            "composing",
        )))
    }

    /// The session's screensaver, hosting savers spawned into the drawing
    /// sandbox.
    type DesktopSaver = Screensaver<RtSessionChannel, tairix_rt::LogSink>;

    /// Spawns a saver bundle into the drawing sandbox, handed a region of its
    /// own the session maps too, and reaped by `reaper` once taken down.
    struct RtSaverLauncher<'a> {
        reaper: &'a alloc::sync::Arc<ChildReaper>,
    }

    impl SaverLauncher for RtSaverLauncher<'_> {
        type Transport = RtSessionChannel;

        fn launch(
            &mut self,
            entry: &str,
            len: usize,
            (memory, threads): (u64, u64),
        ) -> Result<(RtSessionChannel, alloc::boxed::Box<dyn SaverMemory>), Errno> {
            let region = tairix_rt::shm::SharedRegion::create(len).ok_or(Errno::OutOfMemory)?;
            let channel =
                RtSessionChannel::launch_drawing(entry.as_bytes(), memory, threads, region.id())?
                    .reaped_by(alloc::sync::Arc::<ChildReaper>::clone(self.reaper));
            Ok((channel, alloc::boxed::Box::new(OwnRegion(region))))
        }
    }

    /// The children taken down while they still ran — savers and the recorder
    /// — each a child member of the wait set until the kernel reports it
    /// exited and it is reaped: a child given half the machine gives its
    /// memory back in its own time, never on the key that dismissed it, and no
    /// recording's end waits on its recorder's.
    struct ChildReaper {
        set: u64,
        exiting: tairix_rt::sync::Mutex<alloc::vec::Vec<i64>>,
    }

    impl ChildReaper {
        const fn new(set: u64) -> Self {
            Self {
                set,
                exiting: tairix_rt::sync::Mutex::new(alloc::vec::Vec::new()),
            }
        }

        /// Reap every child that has exited, and stop watching it.
        fn collect(&self) {
            self.exiting.lock().retain(|&pid| {
                if tairix_rt::try_reap(pid) == tairix_abi::Reap::Running {
                    return true;
                }
                #[allow(clippy::cast_sign_loss)] // a spawned child's pid is positive.
                let _ = tairix_rt::waitset_ctl(
                    self.set,
                    WaitSetOp::Del,
                    WaitSourceKind::Child,
                    pid as u64,
                    REAPED_TOKEN,
                );
                false
            });
        }
    }

    impl tairix_sandbox::Reaper for ChildReaper {
        fn reap(&self, pid: i64) {
            // One that cannot be watched is still reaped with every other
            // exited child of the session, so it is never waited for here.
            let mut exiting = self.exiting.lock();
            if exiting.try_reserve(1).is_err() {
                return;
            }
            #[allow(clippy::cast_sign_loss)] // a spawned child's pid is positive.
            let watched = tairix_rt::waitset_ctl(
                self.set,
                WaitSetOp::Add,
                WaitSourceKind::Child,
                pid as u64,
                REAPED_TOKEN,
            ) == 0;
            if watched {
                exiting.push(pid);
            }
        }
    }

    /// A region the session made and maps: a saver's, or a recording's slot.
    struct OwnRegion(tairix_rt::shm::SharedRegion);

    impl SaverMemory for OwnRegion {
        fn bytes(&self) -> &[u8] {
            self.0.bytes()
        }

        fn bytes_mut(&mut self) -> &mut [u8] {
            self.0.bytes_mut()
        }
    }

    /// A font call saver `saver` made, in the font service's own wire form.
    struct FontRelay {
        saver: u64,
        request: [u8; tairix_abi::font_ipc::FontRequest::WIRE_LEN],
    }

    /// The font service's answer to a relayed call, and the saver that made it.
    type FontRelayed = (u64, Result<alloc::vec::Vec<u8>, Errno>);

    /// The thread that waits on the font service for a saver, and the buffer
    /// it reads every reply into.
    type SaverFontWorker = Worker<alloc::vec::Vec<u8>, FontRelay, FontRelayed>;

    /// The saver font worker's job: one round trip to the font service, read
    /// into the worker's own buffer, which is sized and zeroed once, and
    /// answered as the bytes it filled.
    fn relay_saver_font(reply: &mut alloc::vec::Vec<u8>, relay: &mut FontRelay) -> FontRelayed {
        const MAX: usize = tairix_abi::saver_ipc::SAVER_FONT_REPLY_MAX;
        if reply.len() < MAX {
            if reply.try_reserve_exact(MAX - reply.len()).is_err() {
                return (relay.saver, Err(Errno::OutOfMemory));
            }
            reply.resize(MAX, 0);
        }
        let answered =
            tairix_rt::ipc_call(tairix_abi::font_ipc::FONT_ENDPOINT, &relay.request, reply)
                .map_err(Errno::from_syscall)
                .and_then(|len| {
                    let filled = reply.get(..len).ok_or(Errno::MessageTooLarge)?;
                    let mut answer = alloc::vec::Vec::new();
                    answer
                        .try_reserve_exact(filled.len())
                        .map_err(|_| Errno::OutOfMemory)?;
                    answer.extend_from_slice(filled);
                    Ok(answer)
                });
        (relay.saver, answered)
    }

    /// What the saver file worker is asked.
    enum SaverFiling {
        /// Keep the picture saver `saver` handed over, named for the moment
        /// in the user's zone.
        Keep {
            saver: u64,
            picture: Picture,
            civil: tairix_tz::CivilClock<'static>,
        },
        /// Read the screensaver stores.
        Scan,
    }

    /// What the saver file worker answers.
    enum SaverFiled {
        /// Saver `saver`'s picture, kept where the path says, or why not.
        Kept {
            saver: u64,
            kept: Result<alloc::string::String, Unkept>,
        },
        /// The savers the stores hold, and why one could not be read where
        /// one could not.
        Scanned((SaverCatalog, Option<tairix_appstore::WalkError>)),
    }

    /// The most saver file jobs waiting at once: a keep for the saver drawing
    /// and one for the saver before it, and a scan.
    const SAVER_FILE_JOBS: usize = 3;

    /// What the saver file worker holds: the account's home, and the watch on
    /// its own screensaver store where there is one.
    struct SaverFiles {
        home: Option<alloc::string::String>,
        store: Option<tairix_rt::Dir>,
    }

    /// The thread that writes savers' pictures and reads the screensaver
    /// stores.
    type SaverFileWorker = Worker<
        SaverFiles,
        SaverFiling,
        SaverFiled,
        tairix_util::defer::JobQueue<SaverFiling, SaverFiled>,
    >;

    /// The saver file worker's job.
    fn file_for_saver(files: &mut SaverFiles, job: &mut SaverFiling) -> SaverFiled {
        match job {
            SaverFiling::Keep {
                saver,
                picture,
                civil,
            } => {
                let when = kept_at(tairix_rt::wall_time().ok(), civil);
                SaverFiled::Kept {
                    saver: *saver,
                    kept: keep_picture(
                        &mut RtKeptFiles,
                        files.home.as_deref(),
                        picture,
                        (civil, when),
                    ),
                }
            }
            SaverFiling::Scan => {
                // What the watch reported is spent before the stores are read,
                // so a change made during the read reports again.
                if let Some(store) = files.store.as_ref() {
                    let mut scratch = [0u8; 2 * tairix_abi::fs::DirChangeBatch::MIN_BUFFER];
                    for _ in 0..SAVER_STORE_DRAIN_BATCHES {
                        let more = store.read_changes(&mut scratch).is_ok_and(|len| {
                            tairix_abi::fs::DirChangeBatch::decode(
                                scratch.get(..len).unwrap_or_default(),
                            )
                            .is_ok_and(|batch| batch.more)
                        });
                        if !more {
                            break;
                        }
                    }
                }
                SaverFiled::Scanned(load_savers(&VfsFileReader, files.home.as_deref()))
            }
        }
    }

    /// The user's own screensaver store in `home`, watched and joined to
    /// `set`, so a saver put there while the desktop runs is listed without
    /// waiting for Settings to ask; `None`, stated, where it cannot be.
    fn watch_saver_store(set: u64, home: Option<&str>) -> Option<tairix_rt::Dir> {
        let roots = tairix_appstore::saver_roots(home);
        let store = roots.get(1)?;
        let watched = tairix_rt::Dir::open(store.as_bytes())
            .and_then(|dir| dir.watch(SAVER_STORE_WATCH_LATENCY_NS).map(|()| dir))
            .map_err(Errno::from_syscall)
            .and_then(|dir| {
                let joined = tairix_rt::waitset_ctl(
                    set,
                    WaitSetOp::Add,
                    WaitSourceKind::DirWatch,
                    u64::from(dir.fd()),
                    SAVER_STORE_TOKEN,
                );
                if joined == 0 {
                    Ok(dir)
                } else {
                    Err(Errno::from_syscall(joined))
                }
            });
        watched
            .map_err(|err| {
                app::report(
                    APP_NAME,
                    format_args!(
                        "a screensaver put in {store} is listed only when Settings next lists \
                         them ({err})"
                    ),
                );
            })
            .ok()
    }

    /// Ask for the screensaver stores to be read again, in place of a read
    /// already waiting. A queue full of pictures to keep leaves the read
    /// `owed`, to be asked again once one of them is done; answers whether it
    /// ran here.
    fn scan_savers(files: &SaverFileWorker, owed: &mut bool) -> bool {
        files.retain_waiting(|job| !matches!(job, SaverFiling::Scan));
        let submitted = files.submit(SaverFiling::Scan);
        *owed = submitted.is_err();
        submitted.unwrap_or(false)
    }

    /// Where a hosted saver's errands go: its font calls to the relay, the
    /// pictures it keeps to the file worker, and the slides it wants to the
    /// wallpaper preparers.
    struct RtSaverErrands<'a> {
        /// The saver asking.
        saver: u64,
        fonts: &'a SaverFontWorker,
        files: &'a SaverFileWorker,
        wallpapers: &'a Wallpapers,
        /// The shipped pictures, the places of those the saver is offered,
        /// and the screen a slide fills.
        slides: (&'a [WallpaperName], &'a [usize], Rect),
        /// The zone a kept picture's moment is named in.
        civil: tairix_tz::CivilClock<'static>,
        owed: OwedErrands,
    }

    /// Answers a saver's errands already have before the saver may be told
    /// them: what a worker carried out on the loop's own thread, or what
    /// nothing could take.
    #[derive(Default)]
    struct OwedErrands {
        fonts: bool,
        files: bool,
        unkept: bool,
        unavailable: Option<u16>,
    }

    impl SaverErrands for RtSaverErrands<'_> {
        fn relay_font(&mut self, request: [u8; tairix_abi::font_ipc::FontRequest::WIRE_LEN]) {
            self.owed.fonts |= self.fonts.submit(FontRelay {
                saver: self.saver,
                request,
            });
        }

        fn keep(&mut self, picture: Picture) {
            match self.files.submit(SaverFiling::Keep {
                saver: self.saver,
                picture,
                civil: self.civil,
            }) {
                Ok(ran_here) => self.owed.files |= ran_here,
                Err(_) => self.owed.unkept = true,
            }
        }

        fn want_slide(&mut self, index: u16) {
            let (catalog, places, screen) = self.slides;
            let source = places
                .get(usize::from(index))
                .and_then(|&place| slide_source(catalog, place, screen));
            let taken = source.is_some_and(|source| {
                self.wallpapers.want_slide(
                    SlideWant {
                        saver: self.saver,
                        index,
                    },
                    source,
                )
            });
            if !taken {
                self.owed.unavailable = Some(index);
            }
        }
    }

    /// Answer saver `instance` what its errands already have.
    fn settle_errands(
        owed: &OwedErrands,
        instance: u64,
        saver: &mut DesktopSaver,
        (fonts, files): (&SaverFontWorker, &SaverFileWorker),
        savers: &mut SaverCatalog,
        compositor: &mut Compositor,
    ) {
        let now_ns = tairix_rt::clock_get();
        if owed.fonts {
            collect_saver_fonts(fonts, saver, compositor);
        }
        if owed.files {
            collect_saver_files(files, saver, savers, compositor);
        }
        if owed.unkept {
            app::report(
                APP_NAME,
                format_args!("a screensaver's picture was not kept: the queue for it is full"),
            );
            if let Some(end) = saver.kept(instance, (false, now_ns), compositor) {
                report_saver_end(&end);
            }
        }
        if let Some(index) = owed.unavailable {
            if let Some(end) = saver.slide_landed(instance, (index, None), now_ns, compositor) {
                report_saver_end(&end);
            }
        }
    }

    /// Hand every relayed font answer that has landed to the saver that
    /// asked.
    fn collect_saver_fonts(
        fonts: &SaverFontWorker,
        saver: &mut DesktopSaver,
        compositor: &mut Compositor,
    ) {
        while let Some((asked, reply)) = fonts.collect() {
            let reply = reply.as_deref().map_err(|errno| *errno);
            if let Some(end) =
                saver.font_answered(asked, (reply, tairix_rt::clock_get()), compositor)
            {
                report_saver_end(&end);
            }
        }
    }

    /// Adopt everything the saver file worker has answered: a kept picture is
    /// said to the saver that kept it, and a reading of the stores becomes the
    /// catalogue, its refusals said where it changed.
    fn collect_saver_files(
        files: &SaverFileWorker,
        saver: &mut DesktopSaver,
        savers: &mut SaverCatalog,
        compositor: &mut Compositor,
    ) {
        files.collect_landed(|filed| match filed {
            SaverFiled::Kept { saver: asked, kept } => {
                if let Err(why) = &kept {
                    app::report(
                        APP_NAME,
                        format_args!("a screensaver's picture was not kept: {why}"),
                    );
                }
                let answered = (kept.is_ok(), tairix_rt::clock_get());
                if let Some(end) = saver.kept(asked, answered, compositor) {
                    report_saver_end(&end);
                }
            }
            SaverFiled::Scanned((catalog, refused)) => {
                if let Some(error) = refused {
                    app::report(
                        APP_NAME,
                        format_args!("the screensaver stores could not be read: {error}"),
                    );
                }
                if catalog != *savers {
                    for saver in catalog.newly_refused(savers) {
                        app::report(
                            APP_NAME,
                            format_args!(
                                "screensaver {} cannot be chosen: {}",
                                saver.bundle,
                                tairix_wallpaper::standing_reason(saver.standing)
                            ),
                        );
                        saver.log_refused(&LOG_SINK);
                    }
                    *savers = catalog;
                }
            }
        });
    }

    /// Bring the wait-set in line with the saver drawing: its pipes watched in
    /// the directions it wants. A saver whose pipes cannot be watched could
    /// never be heard, so it is ended.
    fn watch_saver(
        members: &mut SessionMembers,
        saver: &mut DesktopSaver,
        compositor: &mut Compositor,
    ) {
        let (descriptors, read, write) = saver.saver_descriptors();
        if let Err(errno) = members.sync(descriptors, read, write) {
            if let Some(end) = saver.unwatched(errno, compositor) {
                report_saver_end(&end);
            }
            // The saver has gone, and whatever of its pipes is still watched
            // goes with it.
            let _ = members.sync(None, false, false);
        }
    }

    /// Kept pictures' folders and files, through the kernel VFS under the
    /// session's own kernel-attested identity.
    struct RtKeptFiles;

    impl KeptFiles for RtKeptFiles {
        fn make_folder(&mut self, path: &str) -> Result<(), Errno> {
            match tairix_rt::fs_mkdir(path.as_bytes(), tairix_abi::fs::FS_NEW_DIR_MODE) {
                0.. => Ok(()),
                refused => match Errno::from_syscall(refused) {
                    Errno::AlreadyExists => Ok(()),
                    errno => Err(errno),
                },
            }
        }

        fn write_scratch(&mut self, path: &str, bytes: &[u8]) -> Result<(), Errno> {
            // Creating afresh refuses a name already taken, a link planted
            // there included: what an earlier keep left may be its only copy.
            let file = tairix_rt::create_new(path.as_bytes(), tairix_abi::fs::FS_NEW_FILE_MODE)
                .map_err(Errno::from_syscall)?;
            let written = tairix_rt::fs_write_all(file.fd(), 0, bytes)
                .and_then(|()| file.sync().map_err(Errno::from_syscall));
            if written.is_err() {
                drop(file);
                self.discard(path);
            }
            written
        }

        fn place(&mut self, scratch: &str, path: &str) -> Result<(), Errno> {
            // A name already taken is the kernel's own refusal, so nothing is
            // ever replaced.
            match tairix_rt::fs_rename(
                scratch.as_bytes(),
                path.as_bytes(),
                tairix_abi::RenameFlags::NO_REPLACE,
            ) {
                0.. => Ok(()),
                refused => Err(Errno::from_syscall(refused)),
            }
        }

        fn discard(&mut self, path: &str) {
            let _ = tairix_rt::fs_unlink(path.as_bytes(), tairix_abi::UnlinkFlags::empty());
        }

        fn found(&mut self, path: &str) -> Option<Found> {
            let flags = tairix_abi::OpenFlags::READ.union(tairix_abi::OpenFlags::NO_FOLLOW);
            let stat = tairix_rt::File::open(path.as_bytes(), flags)
                .ok()?
                .stat()
                .ok()?;
            (stat.kind == tairix_abi::fs::FileKind::Regular).then_some(Found {
                changed: stat.times.modified,
                len: stat.size,
            })
        }
    }

    /// A capture's work for the file worker.
    enum CaptureJob {
        /// Keep a still, named for the moment in the user's zone.
        Still {
            shot: StillShot,
            civil: tairix_tz::CivilClock<'static>,
        },
        /// Make a recording's scratch file in `folder`, granted to `recorder`.
        Output {
            folder: KeptFolder,
            recorder: tairix_abi::ProcId,
        },
        /// Settle a recording's scratch file in `folder`: name it, or remove it
        /// where it holds nothing.
        Place {
            folder: KeptFolder,
            scratch: alloc::string::String,
            civil: tairix_tz::CivilClock<'static>,
        },
        /// Name what an earlier session left mid-capture.
        Sweep {
            civil: tairix_tz::CivilClock<'static>,
        },
    }

    /// What the file worker answered a capture.
    enum CaptureFiled {
        Still(Result<StillKept, Unkept>),
        /// The file made for the recorder named, or why it was not made.
        Output((tairix_abi::ProcId, Result<RecordingFile, Unkept>)),
        Placed(Result<Option<alloc::string::String>, Unkept>),
        Swept(alloc::vec::Vec<alloc::string::String>),
    }

    /// The most capture file jobs waiting at once: two stills in flight, a
    /// recording's file and its naming, and the sweep.
    const CAPTURE_FILE_JOBS: usize = 5;

    /// What the capture file worker holds: the account's home.
    struct CaptureFiles {
        home: Option<alloc::string::String>,
    }

    /// The thread that keeps captures and makes and names recordings' files,
    /// each job's answer held in its queue, never left to an allocation.
    type CaptureFileWorker = Worker<
        CaptureFiles,
        CaptureJob,
        CaptureFiled,
        tairix_util::defer::JobQueue<CaptureJob, CaptureFiled>,
    >;

    fn file_for_capture(files: &mut CaptureFiles, job: &mut CaptureJob) -> CaptureFiled {
        let home = files.home.as_deref();
        match job {
            CaptureJob::Still { shot, civil } => {
                let when = kept_at(tairix_rt::wall_time().ok(), civil);
                CaptureFiled::Still(keep_still(&mut RtKeptFiles, home, shot, when))
            }
            CaptureJob::Output { folder, recorder } => {
                CaptureFiled::Output((*recorder, open_recording(home, *folder, *recorder)))
            }
            CaptureJob::Place {
                folder,
                scratch,
                civil,
            } => {
                let when = kept_at(tairix_rt::wall_time().ok(), civil);
                let file = (*folder, scratch.as_str());
                CaptureFiled::Placed(settle_recording(&mut RtKeptFiles, home, file, when))
            }
            CaptureJob::Sweep { civil } => {
                CaptureFiled::Swept(sweep_captures(&mut RtKeptFiles, home, civil))
            }
        }
    }

    /// Claim a recording's scratch file afresh in `folder` and grant it to
    /// `recorder` for its positional writes: the file, with the handle the
    /// recorder redeems it by. A scratch file already there is never taken
    /// over, and the session's own descriptor closes as this returns.
    fn open_recording(
        home: Option<&str>,
        folder: KeptFolder,
        recorder: tairix_abi::ProcId,
    ) -> Result<RecordingFile, Unkept> {
        let (scratch, file) = claim_recording(&mut RtKeptFiles, home, folder, |_, path| {
            tairix_rt::create_new(path.as_bytes(), tairix_abi::fs::FS_NEW_FILE_MODE)
                .map_err(Errno::from_syscall)
        })?;
        let granted = Errno::syscall_result(tairix_rt::fd_grant(
            file.fd(),
            tairix_abi::syscall::GRANT_EXTENT_INHERIT,
            recorder,
            0,
        ));
        drop(file);
        match granted {
            Ok(handle) => Ok(RecordingFile {
                folder,
                scratch,
                handle,
            }),
            Err(errno) => {
                RtKeptFiles.discard(&scratch);
                Err(Unkept::Write(scratch, errno))
            }
        }
    }

    /// The recording the desktop makes, and its pipes as the wait-set watches
    /// them.
    struct Recorder {
        recording: Option<Recording<RtSessionChannel, tairix_rt::LogSink>>,
        members: SessionMembers,
        /// The recorder's process id, which the reap of the session's exited
        /// children reports it by; none for one an administrator allowed,
        /// which is the broker's child.
        child: Option<u64>,
        /// Whether the bar shows the indicator.
        shown: bool,
        /// The broker's answer to whether the account may record the
        /// recording's sound itself, for the capture's turn to take up.
        heard: Option<AuthoriseAnswer>,
        /// What the administrator's prompt for its sound came to.
        allowed: Option<Allowed>,
    }

    impl Recorder {
        const fn new(set: u64) -> Self {
            Self {
                recording: None,
                members: SessionMembers::new(set, RECORDER_READ_TOKEN, RECORDER_WRITE_TOKEN),
                child: None,
                shown: false,
                heard: None,
                allowed: None,
            }
        }

        /// Keep `answer` for the recording that asked it, or hand it back.
        fn hear(&mut self, answer: AuthoriseAnswer) -> Option<AuthoriseAnswer> {
            let asked = self
                .recording
                .as_ref()
                .is_some_and(|recording| recording.asked(answer.ticket()));
            if !asked {
                return Some(answer);
            }
            self.heard = Some(answer);
            None
        }

        /// The administrator's prompt for the recording's sound ended: with
        /// the recorder they started, or with nothing.
        fn administrator_answered(&mut self, wired: Option<WiredLaunch>) {
            let allowed = wired.map_or(Allowed::Declined, Allowed::Administrator);
            if let Some(Allowed::Administrator(stale)) = self.allowed.replace(allowed) {
                let_go_recorder(stale);
            }
        }

        /// Tell the recording whose recorder the session's reap of its exited
        /// children took how it exited: what a recorder that never started is
        /// said to have ended by.
        fn reaped(&mut self, pid: u64, status: WaitStatus) {
            let WaitStatus::Exited(code) = status else {
                return;
            };
            if self.child != Some(pid) {
                return;
            }
            self.child = None;
            if let Some(recording) = self.recording.as_mut() {
                recording.exited(code);
            }
        }

        /// Watch the recorder's pipes in the directions it wants, or none once
        /// it has gone. One the wait-set refuses is left to its deadlines.
        fn watch(&mut self) {
            let wanted = self
                .recording
                .as_ref()
                .map_or((None, false, false), |recording| {
                    (
                        recording.descriptors(),
                        recording.wants_read(),
                        recording.wants_write(),
                    )
                });
            if self.members.sync(wanted.0, wanted.1, wanted.2).is_err() {
                let _ = self.members.sync(None, false, false);
            }
        }
    }

    /// `recorder.app`, started under the authority its own signed manifest
    /// grants, over two pipes only the session holds, its slots regions of
    /// the session's granted to its process alone, and reaped by `reaper`
    /// should it outlive its recording.
    struct RtRecorderHost<'a> {
        reaper: &'a alloc::sync::Arc<ChildReaper>,
        /// The session's own instance, which a recorder it starts is told to
        /// map its slots from.
        session: tairix_abi::ProcId,
        /// The process id of the recorder the last launch started.
        started: Option<u64>,
    }

    impl RecorderHost for RtRecorderHost<'_> {
        type Transport = RtSessionChannel;

        fn launch(&mut self) -> Result<(RtSessionChannel, tairix_abi::ProcId), Errno> {
            let reaper = alloc::sync::Arc::<ChildReaper>::clone(self.reaper);
            let path = tairix_abi::recorder_ipc::RECORDER_RUN_PATH.as_bytes();
            let mut hex = [0u8; tairix_abi::PROC_ID_HEX_LEN];
            let session = self.session.write_hex(&mut hex).as_bytes();
            let (channel, recorder) =
                RtSessionChannel::launch_component(path, &[path, session], reaper)?;
            self.started = Some(recorder.pid());
            Ok((channel, recorder.proc_id()))
        }

        fn adopt(
            &mut self,
            launch: WiredLaunch,
        ) -> Result<(RtSessionChannel, tairix_abi::ProcId), Errno> {
            let channel = RtSessionChannel::redeemed(launch.input, launch.output)?;
            Ok((channel, launch.instance))
        }

        fn grant_slot(
            &mut self,
            recorder: tairix_abi::ProcId,
            len: usize,
        ) -> Result<(u64, alloc::boxed::Box<dyn SaverMemory>), Errno> {
            let region = tairix_rt::shm::SharedRegion::create(len).ok_or(Errno::OutOfMemory)?;
            let handle = Errno::syscall_result(tairix_rt::shm_grant_to(region.id(), recorder))?;
            Ok((handle, alloc::boxed::Box::new(OwnRegion(region))))
        }
    }

    /// Let go of the recorder an administrator allowed that no recording takes
    /// up: its pipes are redeemed only to be closed, which ends it, and the
    /// broker reaps it.
    fn let_go_recorder(launch: WiredLaunch) {
        drop(RtSessionChannel::redeemed(launch.input, launch.output));
    }

    /// What the capture's turn reads of the session.
    struct CaptureTurn<'a> {
        /// The session's own instance.
        session: tairix_abi::ProcId,
        screened: Screened,
        /// The lock, a change of user or the session's going holds the screen.
        away: bool,
        saver: bool,
        files: &'a CaptureFileWorker,
        /// What a recorder still running when its recording ends is reaped by.
        reaper: &'a alloc::sync::Arc<ChildReaper>,
        clipboard: &'a mut tairix_desktop_session::clipboard::SessionClipboard<RtPayloadRegions>,
        producer: Option<&'a tairix_taskbar::Producer>,
        /// The one authorisation prompt, which a recording's sound is asked
        /// through, and the broker it asks.
        elevate: &'a mut AuthorisePrompt,
        elevator: &'a mut RtElevator,
    }

    /// One turn of the screen capture: what hides the screen, the work the
    /// controller left, the file worker's answers, the recording served, and
    /// the indicator.
    fn serve_capture(
        (capture, recorder): (&mut CaptureController, &mut Recorder),
        (shell, compositor): (&mut DesktopShell, &mut Compositor),
        turn: &mut CaptureTurn<'_>,
    ) {
        let now_ns = tairix_rt::clock_get();
        capture.screen(turn.screened);
        if turn.away || turn.saver || turn.screened.sheet.is_some() {
            capture.abandon(shell, compositor);
        }
        capture.tick(now_ns, shell, compositor);
        let step = recorder
            .recording
            .as_mut()
            .and_then(|recording| recording.screen(turn.screened, turn.saver, compositor, now_ns));
        let screen = (&mut *shell, &mut *compositor);
        settle_record_steps(step, (&mut *capture, &mut *recorder), turn, screen, now_ns);
        let asked = capture.take_stop();
        let stopping = if turn.away {
            Some(StopReason::Screened)
        } else {
            asked.then_some(StopReason::Asked)
        };
        if let Some(reason) = stopping {
            let step = recorder
                .recording
                .as_mut()
                .and_then(|recording| recording.stop(reason, compositor, now_ns));
            let screen = (&mut *shell, &mut *compositor);
            settle_record_steps(step, (&mut *capture, &mut *recorder), turn, screen, now_ns);
        }
        let screen = (&mut *shell, &mut *compositor);
        take_up_sound((&mut *capture, &mut *recorder), turn, screen, now_ns);
        for work in capture.take_work() {
            match work {
                CaptureWork::Still(shot) => {
                    let job = CaptureJob::Still {
                        shot,
                        civil: shell.clock().civil(),
                    };
                    if turn.files.submit(job).is_err() {
                        let busy = CaptureNotice::Unkept(Unkept::Busy);
                        raise_capture_notice(&busy, turn.producer, shell, compositor);
                    }
                }
                CaptureWork::Record(plan) => {
                    let held = (&mut *capture, &mut *recorder);
                    start_recording(plan, held, turn, compositor, now_ns, shell);
                }
                CaptureWork::Notice(notice) => {
                    raise_capture_notice(&notice, turn.producer, shell, compositor);
                }
            }
        }
        // An answer or a step can make more of either: a job carried out here
        // for want of a worker lands at once, and a recording answered serves
        // again.
        loop {
            let mut acted = false;
            let files = turn.files;
            files.collect_landed(|answer| {
                acted = true;
                let screen = (&mut *shell, &mut *compositor);
                let step = take_capture_answer(answer, recorder, turn, screen, now_ns);
                let screen = (&mut *shell, &mut *compositor);
                settle_record_steps(step, (&mut *capture, &mut *recorder), turn, screen, now_ns);
            });
            let mut host = RtRecorderHost {
                reaper: turn.reaper,
                session: turn.session,
                started: None,
            };
            let step = recorder
                .recording
                .as_mut()
                .and_then(|recording| recording.serve(&mut host, compositor, now_ns));
            if step.is_none() && !acted {
                break;
            }
            let screen = (&mut *shell, &mut *compositor);
            settle_record_steps(step, (&mut *capture, &mut *recorder), turn, screen, now_ns);
        }
        let live = recorder.recording.as_ref().is_some_and(Recording::is_live);
        capture.set_recording(live);
        if live != recorder.shown {
            recorder.shown = live;
            shell.set_screen_recording(compositor, live);
        }
    }

    /// Start the recording `plan` asks for: at once when it is silent, and
    /// once its sound is allowed when it is not, the account's own leave asked
    /// of the broker off the loop first.
    fn start_recording(
        plan: RecordPlan,
        (capture, recorder): (&mut CaptureController, &mut Recorder),
        turn: &mut CaptureTurn<'_>,
        compositor: &mut Compositor,
        now_ns: u64,
        shell: &mut DesktopShell,
    ) {
        if recorder.recording.is_some() {
            raise_capture_notice(&CaptureNotice::Finishing, turn.producer, shell, compositor);
            return;
        }
        let mut host = RtRecorderHost {
            reaper: turn.reaper,
            session: turn.session,
            started: None,
        };
        let home = home_dir();
        let held = (tairix_rt::LogSink, turn.screened);
        let started = if plan.sound == tairix_abi::recorder_ipc::RecordSound::None {
            Recording::start(&mut host, plan, held, home.as_deref(), compositor, now_ns)
        } else {
            Recording::authorising(plan, held, home.as_deref(), &mut *turn.elevator)
        };
        match started {
            Ok(recording) => {
                if recording.is_live() {
                    capture.ring_clicks(recording.shows_clicks());
                }
                recorder.recording = Some(recording);
                recorder.child = host.started;
            }
            Err(lost) => {
                raise_capture_notice(&CaptureNotice::Lost(lost), turn.producer, shell, compositor);
            }
        }
    }

    /// Take up what a recording's sound came to — the broker's answer whether
    /// its account may record it, then an administrator's — starting its
    /// recorder where one may be. An administrator's recorder no recording
    /// waits on is let go.
    fn take_up_sound(
        (capture, recorder): (&mut CaptureController, &mut Recorder),
        turn: &mut CaptureTurn<'_>,
        (shell, compositor): (&mut DesktopShell, &mut Compositor),
        now_ns: u64,
    ) {
        if recorder.heard.is_none() && recorder.allowed.is_none() {
            return;
        }
        let mut host = RtRecorderHost {
            reaper: turn.reaper,
            session: turn.session,
            started: None,
        };
        let heard = recorder.heard.take().and_then(|answer| {
            let recording = recorder.recording.as_mut()?;
            recording.answered(&answer, &mut host, compositor, now_ns)
        });
        let allowed = recorder.allowed.take().and_then(|allowed| {
            if let Some(recording) = recorder.recording.as_mut() {
                recording.authorised(&mut host, allowed, compositor, now_ns)
            } else {
                if let Allowed::Administrator(launch) = allowed {
                    let_go_recorder(launch);
                }
                None
            }
        });
        if host.started.is_some() {
            recorder.child = host.started;
        }
        let live = recorder
            .recording
            .as_ref()
            .filter(|recording| recording.is_live());
        if let Some(recording) = live {
            capture.ring_clicks(recording.shows_clicks());
        }
        for step in [heard, allowed] {
            let screen = (&mut *shell, &mut *compositor);
            settle_record_steps(step, (&mut *capture, &mut *recorder), turn, screen, now_ns);
        }
    }

    /// Act on what the file worker answered a capture, answering the step a
    /// recording given its output takes.
    fn take_capture_answer(
        answer: CaptureFiled,
        recorder: &mut Recorder,
        turn: &mut CaptureTurn<'_>,
        (shell, compositor): (&mut DesktopShell, &mut Compositor),
        now_ns: u64,
    ) -> Option<RecordStep> {
        let notice = match answer {
            CaptureFiled::Still(Ok(StillKept::Clipboard(png))) => turn
                .clipboard
                .hold_png(png.into_bytes())
                .err()
                .map(|errno| {
                    CaptureNotice::Unkept(Unkept::Write(
                        alloc::string::String::from("the clipboard"),
                        errno,
                    ))
                }),
            CaptureFiled::Still(Ok(StillKept::Oversize(path))) => {
                log_kept(shell, Kept::Still);
                Some(CaptureNotice::SavedInstead(path))
            }
            CaptureFiled::Still(Ok(StillKept::Saved(_))) => {
                log_kept(shell, Kept::Still);
                None
            }
            CaptureFiled::Placed(Ok(named)) => {
                if named.is_some() {
                    log_kept(shell, Kept::Recording);
                }
                None
            }
            CaptureFiled::Still(Err(why)) | CaptureFiled::Placed(Err(why)) => {
                Some(CaptureNotice::Unkept(why))
            }
            CaptureFiled::Output(made) => {
                return take_output(recorder.recording.as_mut(), made, compositor, now_ns);
            }
            CaptureFiled::Swept(kept) => {
                for path in kept {
                    app::report(
                        APP_NAME,
                        format_args!("a capture cut short was kept as {path}"),
                    );
                    log_kept(shell, Kept::Leftover);
                }
                None
            }
        };
        if let Some(notice) = notice {
            raise_capture_notice(&notice, turn.producer, shell, compositor);
        }
        None
    }

    /// Settle `step` and every step settling it leads to.
    fn settle_record_steps(
        mut step: Option<RecordStep>,
        (capture, recorder): (&mut CaptureController, &mut Recorder),
        turn: &mut CaptureTurn<'_>,
        (shell, compositor): (&mut DesktopShell, &mut Compositor),
        now_ns: u64,
    ) {
        while let Some(next) = step.take() {
            let held = (&mut *capture, &mut *recorder);
            step = settle_record_step(next, held, turn, (&mut *shell, &mut *compositor), now_ns);
        }
    }

    /// Act on what the recording asked: its file made, or its end settled —
    /// named, its loss stated, its thumbnail shown — and a file no recording
    /// waits on settled too. A file the worker had no room to make is answered
    /// at once, ending the recording with the reason.
    fn settle_record_step(
        step: RecordStep,
        (capture, recorder): (&mut CaptureController, &mut Recorder),
        turn: &mut CaptureTurn<'_>,
        (shell, compositor): (&mut DesktopShell, &mut Compositor),
        now_ns: u64,
    ) -> Option<RecordStep> {
        match step {
            RecordStep::WantOutput {
                recorder: process,
                folder,
            } => {
                let job = CaptureJob::Output {
                    folder,
                    recorder: process,
                };
                if turn.files.submit(job).is_ok() {
                    return None;
                }
                let refused = (process, Err(Unkept::Busy));
                take_output(recorder.recording.as_mut(), refused, compositor, now_ns)
            }
            RecordStep::Ended(end) => {
                if let Some(recording) = recorder.recording.take() {
                    if end.kept {
                        capture.show_thumbnail(recording.picture(), now_ns, shell, compositor);
                    }
                }
                recorder.child = None;
                capture.ring_clicks(false);
                if let Some(scratch) = end.scratch {
                    name_recording(end.folder, scratch, turn, shell);
                }
                if let Some(lost) = end.lost {
                    let notice = CaptureNotice::Lost(lost);
                    raise_capture_notice(&notice, turn.producer, shell, compositor);
                }
                None
            }
            RecordStep::Unused(file) => {
                name_recording(file.folder, file.scratch, turn, shell);
                None
            }
            RecordStep::Authorise(request) => {
                if turn
                    .elevate
                    .ask(request, &mut *turn.elevator, shell, compositor)
                {
                    return None;
                }
                // The one prompt is taken, or cannot be shown: nobody can
                // allow it now.
                let mut host = RtRecorderHost {
                    reaper: turn.reaper,
                    session: turn.session,
                    started: None,
                };
                recorder.recording.as_mut().and_then(|recording| {
                    recording.authorised(&mut host, Allowed::Declined, compositor, now_ns)
                })
            }
        }
    }

    /// Have the file worker settle `scratch`, a recording's file in `folder`:
    /// named, or removed where it holds nothing.
    fn name_recording(
        folder: KeptFolder,
        scratch: alloc::string::String,
        turn: &CaptureTurn<'_>,
        shell: &DesktopShell,
    ) {
        let job = CaptureJob::Place {
            folder,
            scratch,
            civil: shell.clock().civil(),
        };
        if turn.files.submit(job).is_err() {
            app::report(
                APP_NAME,
                format_args!("a recording is named at the next start"),
            );
        }
    }

    /// Say `notice` on `stderr` and in the notification area, as the desktop
    /// itself.
    fn raise_capture_notice(
        notice: &CaptureNotice,
        producer: Option<&tairix_taskbar::Producer>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        let (title, body) = notice.text();
        app::report(APP_NAME, format_args!("{title}: {body}"));
        let Some(producer) = producer else {
            return;
        };
        let note = tairix_taskbar::TransientNotification::new(
            *producer,
            CAPTURE_NOTICE_KEY,
            tairix_taskbar::NotifySeverity::Warning,
            title,
            body,
        );
        let raised = shell.session_mut().taskbar_mut().raise_notification(note);
        if raised.is_ok_and(tairix_taskbar::Raised::changed) {
            shell.present(compositor);
        }
    }

    /// The serve loop's own parser-sandbox worker: this binary re-entered as a
    /// capability-empty child, which an untrusted image is decoded in.
    ///
    /// Shared behind an `Rc` because the loop's pinboard state and, where no
    /// decoder thread was granted, the shell's boxed resolver both need the very
    /// same live worker — and *not* `Send`, deliberately: each worker thread
    /// creates its own rather than borrowing this one, so a sandbox handle never
    /// crosses a thread. On a desktop that got its threads this is the fallback
    /// path only, and its child is never even spawned.
    type SharedSandbox =
        alloc::rc::Rc<core::cell::RefCell<ParserSandbox<RtLauncher, tairix_rt::LogSink>>>;

    /// The loop's own [`ArtworkRasteriser`]: untrusted icon bytes go to the
    /// parser-sandbox icon service — this binary re-entered as a
    /// capability-empty worker — and only a verified pixel block comes
    /// back. Any refusal (malformed image, crashed worker, unavailable
    /// spawn) is `None`: the slot falls back to its class glyph.
    struct SandboxRasteriser {
        sandbox: SharedSandbox,
    }

    impl ArtworkRasteriser for SandboxRasteriser {
        fn rasterise(&mut self, side: u32, icon: &[u8]) -> Option<alloc::vec::Vec<u8>> {
            rasterise_icon(
                &mut self.sandbox.borrow_mut(),
                side,
                icon,
                &mut tairix_font::ServiceFonts::new(),
            )
            .ok()
        }
    }

    /// The same decode over a sandbox worker the *calling thread* owns
    /// outright.
    ///
    /// [`SandboxRasteriser`] shares the serve loop's handle behind an `Rc`,
    /// which a thread cannot take and must not: the artwork worker builds one
    /// of these instead, exactly as the wallpaper worker builds its own.
    struct OwnedSandbox(ParserSandbox<RtLauncher, tairix_rt::LogSink>);

    impl ArtworkRasteriser for OwnedSandbox {
        fn rasterise(&mut self, side: u32, icon: &[u8]) -> Option<alloc::vec::Vec<u8>> {
            rasterise_icon(
                &mut self.0,
                side,
                icon,
                &mut tairix_font::ServiceFonts::new(),
            )
            .ok()
        }

        fn thumbnail(
            &mut self,
            side: u32,
            document: &mut dyn tairix_icon::ArtworkDocument,
        ) -> Option<tairix_icon::Fitted> {
            tairix_sandbox::imagerender::thumbnail(&mut self.0, side, document).ok()
        }

        fn application_sprite(
            &mut self,
            side: u32,
            names: &tairix_icon::SpriteNames,
            document: &mut dyn tairix_icon::ArtworkDocument,
        ) -> Option<tairix_icon::Fitted> {
            tairix_sandbox::imagerender::application_sprite(&mut self.0, side, names, document).ok()
        }

        fn boot_sprite_files(&mut self, boot: &[u8]) -> Option<Vec<alloc::string::String>> {
            tairix_sandbox::imagerender::boot_sprite_files(&mut self.0, boot).ok()
        }
    }

    /// The session's pinboard state, kept beside the loop: the loop's own
    /// sandbox worker (the wallpaper's fallback when no thread was granted),
    /// and what the wallpaper surface now on screen was prepared from.
    ///
    /// The backdrop menu is *not* here: it is the seat's one menu chain like
    /// every other menu on the desktop, so the pinboard hands over a model and
    /// keeps no shell of its own.
    ///
    /// The settings themselves are *not* here: the desktop model owns them,
    /// so there is exactly one copy of what is in force. Nor is the store —
    /// it is the application's own published app-data scope, opened for the
    /// one round trip a read or a publish costs and never held between them,
    /// so there is no handle here that could go stale against what the
    /// service holds.
    struct PinboardPanel {
        sandbox: SharedSandbox,
        prepared: Option<WallpaperSource>,
    }

    /// What a chosen row of one of the **desktop's own** menus acts on,
    /// bundled so it reaches the chain's one delivery point without a dozen
    /// more parameters.
    ///
    /// Only a desktop-owned chain reads it. An application's chain is answered
    /// over the window channel and touches none of this.
    ///
    /// The backdrop's rows act on the desktop model directly. The icon bar's
    /// resolve to the same typed [`TaskbarResponse`] a click on the bar
    /// produces, so they leave through `answered` and are routed exactly where
    /// every other bar outcome is — there is no second place a *Log Out* row
    /// and a *Log Out* click are honoured.
    struct DesktopMenuDesk<'a, S: DirectorySource> {
        pinboard: &'a mut PinboardPanel,
        wallpapers: &'a Wallpapers,
        publisher: &'a Publisher,
        catalogs: &'a Catalogs,
        files: &'a Listings,
        desktop: &'a mut Desktop<S>,
        launched: &'a mut LaunchTable,
        programs: &'a mut Programs,
        /// The outcomes the bar's own chains resolved to, in the order they
        /// were chosen.
        answered: &'a mut Vec<tairix_desktop_session::ShellOutcome>,
        capture: &'a mut CaptureController,
    }

    /// Everything a drained seat event reaches beyond the [`Seat`] itself:
    /// the window channel, the launch table, the prompts and the session
    /// authority.
    struct SessionRoute<'a, S: DirectorySource, F: PickSources<S>> {
        publisher: &'a Publisher,
        catalogs: &'a Catalogs,
        files: &'a Listings,
        pinboard: &'a mut PinboardPanel,
        wallpapers: &'a Wallpapers,
        desktop: &'a mut Desktop<S>,
        windows: &'a mut SessionWindows,
        focused: &'a mut Option<u64>,
        server: &'a mut WindowServer<RtShmMapper>,
        sink: &'a mut RtEventSink,
        picker: &'a mut SessionPicker<S, F>,
        confirm: &'a mut ConfirmPrompt,
        elevate: &'a mut AuthorisePrompt,
        elevator: &'a mut RtElevator,
        resolution: &'a mut ResolutionPrompt,
        account: &'a str,
        shown_name: &'a str,
        identity: &'a RtWindowIdentity,
        launched: &'a mut LaunchTable,
        apps: &'a mut AppBarPanel,
        switchboard_pid: &'a mut Option<u64>,
        pending_open: &'a mut Option<CommandSection>,
        programs: &'a mut Programs,
        switch: &'a mut SwitchUser,
        display: &'a mut Option<Ring>,
        fade: &'a mut ScreenFade,
        set: u64,
        sound: &'a mut SoundSession,
        sound_worker: &'a SoundWorker,
        /// Every call the session posts to the cue authority, the volume's
        /// feedback among them.
        cue_posts: &'a mut tairix_soundtheme::PostedCues,
        system: &'a SystemFacts,
        /// The account's record of its program library.
        memory: &'a mut LauncherMemory,
    }

    impl<S: DirectorySource, F: PickSources<S>> SessionRoute<'_, S, F> {
        /// Tell the window the event just routed took the pointer from, as
        /// that event is routed, so the leave reaches it before anything the
        /// pointer does next.
        fn tell_pointer_left(&mut self, seat: &mut Seat<'_>) {
            if let Some(gone) = seat.shell.take_client_left() {
                tell_pointer_gone(
                    gone,
                    self.server,
                    self.sink,
                    seat.shell,
                    seat.compositor,
                    self.windows,
                    self.picker,
                    &mut self.apps.service,
                    seat.menu,
                );
            }
        }
    }

    impl<S: DirectorySource, F: PickSources<S>> SeatRouter for SessionRoute<'_, S, F> {
        fn route(
            &mut self,
            seat: &mut Seat<'_>,
            outcome: Timed<tairix_desktop_session::ShellOutcome>,
            key: Option<KeyInput>,
            now_ns: u64,
        ) -> Routed {
            if let tairix_desktop_session::ShellOutcome::Taskbar(TaskbarResponse::Sound(action)) =
                &outcome.value
            {
                let (device_id, control, settled) = match *action {
                    SoundAction::Level {
                        device_id,
                        level,
                        settled,
                    } => (device_id, DeviceControl::Level(level), settled),
                    SoundAction::Mute { device_id, muted } => {
                        (device_id, DeviceControl::Mute(muted), true)
                    }
                };
                self.sound.ask(device_id, control, settled);
                pump_sound(
                    self.sound,
                    self.sound_worker,
                    self.cue_posts,
                    self.set,
                    self.publisher,
                    self.pinboard,
                    self.wallpapers,
                    self.desktop,
                    seat.shell,
                    seat.compositor,
                );
            }
            self.carry_desktop_over_window(seat);
            let refused = route_desktop(
                &outcome,
                self.publisher,
                self.files,
                self.pinboard,
                self.wallpapers,
                self.desktop,
                seat.shell,
                seat.compositor,
                self.windows,
                seat.menu,
                tairix_desktop_session::seat_modal(
                    seat.lock,
                    (self.confirm, self.elevate, self.resolution),
                ),
                &mut LaunchCtx {
                    launched: self.launched,
                    apps: &self.apps.service,
                    server: self.server,
                    sink: self.sink,
                    windows: self.windows,
                    identity: self.identity,
                },
                self.programs,
                now_ns,
            );
            if let Some(refusal) = refused {
                state_refusal(&refusal, self.windows, seat.shell, seat.compositor);
            }
            let routed = route_outcome(
                outcome,
                key,
                self.catalogs,
                (self.files, self.desktop),
                self.focused,
                seat.shell,
                seat.compositor,
                self.windows,
                self.server,
                self.sink,
                self.picker,
                (self.confirm, (self.elevate, self.elevator), self.resolution),
                seat.lock,
                seat.menu,
                self.account,
                self.shown_name,
                self.identity,
                self.launched,
                self.apps,
                self.switchboard_pid,
                self.pending_open,
                self.programs,
                self.system,
                self.memory,
            );
            reconcile_focus(
                self.focused,
                self.server,
                self.sink,
                seat.shell,
                seat.compositor,
                self.windows,
                self.picker,
                &mut self.apps.service,
                seat.menu,
            );
            self.tell_pointer_left(seat);
            routed
        }

        fn settle_chain(
            &mut self,
            seat: &mut Seat<'_>,
            answered: &mut Vec<tairix_desktop_session::ShellOutcome>,
            now_ns: u64,
        ) {
            present_menu_chain(seat.menu, seat.shell, seat.compositor, self.windows);
            answer_menu_chain(
                seat.menu,
                seat.shell,
                seat.compositor,
                self.windows,
                self.server,
                self.sink,
                self.picker,
                &mut self.apps.service,
                self.identity,
                &mut DesktopMenuDesk {
                    pinboard: self.pinboard,
                    wallpapers: self.wallpapers,
                    publisher: self.publisher,
                    catalogs: self.catalogs,
                    files: self.files,
                    desktop: self.desktop,
                    launched: self.launched,
                    programs: self.programs,
                    answered,
                    capture: &mut *seat.capture,
                },
                now_ns,
            );
        }

        fn step_aside(&mut self, seat: &mut Seat<'_>) -> bool {
            step_aside(
                self.switch,
                SessionDeparture {
                    display: self.display,
                    compositor: seat.compositor,
                    fade: self.fade,
                    set: self.set,
                },
            )
        }

        fn drag_place(&mut self, seat: &mut Seat<'_>, at: tairix_wm::Point) -> DragPlace {
            let wm = seat.compositor.window_at(at);
            if seat.shell.carries_desktop_icons() {
                // Brought back over the desktop they are put down, never
                // dropped into one of its folders.
                return match wm {
                    Some(wm) => self.offer_window(seat.compositor, None, wm, at),
                    None => DragPlace::Desktop {
                        icon: None,
                        revision: self.desktop.revision(),
                    },
                };
            }
            if let Some(saving) = seat.shell.save_source() {
                if let Some(wm) = wm {
                    return self.offer_window(seat.compositor, Some(saving), wm, at);
                }
            } else {
                let Some((source, _)) = seat.shell.drag_items() else {
                    return DragPlace::Nothing;
                };
                let scale = seat.compositor.scale();
                if let Some(index) = seat.shell.session().taskbar().app_slot_at(at, scale) {
                    return DragPlace::Slot { index };
                }
                if let Some(wm) = wm {
                    return self.drag_window(seat.compositor, source, wm, at);
                }
            }
            let layout = seat.shell.desktop_layout(seat.compositor, self.desktop);
            DragPlace::Desktop {
                icon: self.desktop.drop_icon_at(
                    at,
                    &layout,
                    seat.compositor.scale(),
                    seat.shell.session().active_theme(),
                ),
                revision: self.desktop.revision(),
            }
        }

        fn drag_surface(&mut self, seat: &mut Seat<'_>, place: DragPlace) -> DragSurface {
            match place {
                DragPlace::Nothing => DragSurface::Nothing,
                DragPlace::Slot { index } => DragSurface::Slot {
                    index,
                    answer: seat
                        .shell
                        .drag_items()
                        .and_then(|(_, items)| self.slot_answer(index, items)),
                },
                // Asked once as the pointer arrives on the window, and carried
                // with it as it moves there.
                DragPlace::AppWindow {
                    index,
                    window_id,
                    x,
                    y,
                } => DragSurface::AppWindow {
                    index,
                    answer: seat.shell.take_window_answer(window_id).unwrap_or_else(|| {
                        seat.shell
                            .drag_items()
                            .and_then(|(_, items)| self.slot_answer(index, items))
                    }),
                    window_id,
                    x,
                    y,
                },
                DragPlace::Window { window_id, x, y } => DragSurface::Window { window_id, x, y },
                DragPlace::Offer { window_id, x, y } => DragSurface::Offer { window_id, x, y },
                DragPlace::Desktop { icon, revision } => {
                    let folder = self.desktop.drop_folder(icon);
                    // What is carried lands in that folder, standing where it
                    // is let go when that is the bare desktop.
                    let offer = seat
                        .shell
                        .drag_items()
                        .map(|(_, items)| LandingOffer::of(items, folder.clone(), icon.is_none()));
                    self.desktop.offer_landing(offer);
                    DragSurface::Desktop {
                        folder,
                        icon,
                        revision,
                    }
                }
            }
        }

        fn report_drag(&mut self, seat: &mut Seat<'_>, report: OwedReport) {
            let (left, over, file) = match report {
                OwedReport::Items(report) => {
                    let at = match &report.at {
                        &DragSurface::Window { window_id, x, y } => {
                            DragReport::Window { window_id, x, y }
                        }
                        DragSurface::Desktop { folder, .. } => DragReport::Desktop { folder },
                        DragSurface::Nothing
                        | DragSurface::Slot { .. }
                        | DragSurface::AppWindow { .. }
                        | DragSurface::Offer { .. } => DragReport::Nowhere,
                    };
                    // A report the engine refuses — the window under the
                    // pointer has just closed — draws no answer, so the
                    // pointer shows none and a drop there drops nothing.
                    let _ = self.server.report_drag(
                        self.sink,
                        report.source,
                        report.serial,
                        at,
                        report.shift,
                    );
                    (None, None, None)
                }
                OwedReport::Save(report) => (
                    report.left.map(OfferEnd::event),
                    report.over.map(SaveOffer::event),
                    None,
                ),
                OwedReport::Desktop(report) => (
                    report.left.map(ItemsOfferEnd::event),
                    report.over.map(ItemsOffer::event),
                    report.file.map(FileOffer::event),
                ),
                OwedReport::File(report) => (
                    report.left.map(ItemsOfferEnd::event),
                    report.over.map(FileOffer::event),
                    None,
                ),
            };
            for event in left.iter().chain(over.iter()).chain(file.iter()) {
                deliver(
                    self.server,
                    self.sink,
                    seat.shell,
                    seat.compositor,
                    self.windows,
                    self.picker,
                    &mut self.apps.service,
                    seat.menu,
                    event,
                );
            }
            light_drop_icon(seat.shell, seat.compositor, self.desktop);
        }

        fn settle_drag(&mut self, seat: &mut Seat<'_>, ended: DragEnd, now_ns: u64) {
            let ended = match ended {
                DragEnd::Items(ended) => ended,
                DragEnd::Save(end) => {
                    self.settle_save(seat, end);
                    return;
                }
                DragEnd::Desktop(end) => {
                    self.settle_desktop(seat, end, now_ns);
                    return;
                }
            };
            if let Some(refusal) = &ended.refused {
                state_drop_refusal(refusal, self.windows, seat.shell, seat.compositor);
            }
            if let Some(end) = ended.offer {
                deliver(
                    self.server,
                    self.sink,
                    seat.shell,
                    seat.compositor,
                    self.windows,
                    self.picker,
                    &mut self.apps.service,
                    seat.menu,
                    &end.event(),
                );
            }
            let on_desktop = if let DragConclusion::Desktop { serial, .. } = ended.ended {
                Some(serial)
            } else {
                None
            };
            let Some(owner) = self.server.owner_of(ended.source) else {
                self.land_drop(seat, ended.source, (None, None));
                return;
            };
            let concluded = self
                .server
                .conclude_drag(self.sink, ended.source, ended.ended);
            let landed = on_desktop.filter(|_| concluded.is_ok());
            let displaced = concluded.as_ref().ok().copied().flatten();
            self.land_drop(seat, ended.source, (landed, displaced));
            let concluded = match concluded {
                // A window dropped on that has closed is a drop on nothing.
                Err(Errno::OutOfRange) => {
                    self.server
                        .conclude_drag(self.sink, ended.source, DragConclusion::Nothing)
                }
                other => other,
            };
            if let Err(Errno::NotFound) = concluded {
                drop_departed(
                    owner,
                    self.server,
                    seat.shell,
                    seat.compositor,
                    self.windows,
                    self.picker,
                    &mut self.apps.service,
                    seat.menu,
                );
            }
            light_drop_icon(seat.shell, seat.compositor, self.desktop);
        }

        fn attend(&mut self, seat: &mut Seat<'_>, now_ns: u64) {
            if !self.elevate.attend(now_ns, seat.shell, seat.compositor) {
                io::write_stderr_line("desktop: the secure attention notice could not be shown");
            }
        }
    }

    impl<S: DirectorySource, F: PickSources<S>> SessionRoute<'_, S, F> {
        /// Have what a drop on the desktop carried land as window `source`'s
        /// application makes it — only where that application was told the
        /// drop numbered `landed` reached the desktop — end the landing the drop
        /// `displaced` past the window's bound, and forget what the drag
        /// offered.
        fn land_drop(
            &mut self,
            seat: &mut Seat<'_>,
            source: u64,
            (landed, displaced): (Option<u32>, Option<u32>),
        ) {
            if let Some(serial) = displaced {
                self.windows.end_landing(source, serial);
            }
            if let Some(serial) = landed {
                let layout = seat.shell.desktop_layout(seat.compositor, self.desktop);
                self.desktop.expect_drop(
                    (source, serial),
                    seat.shell.router().pointer(),
                    &layout,
                    seat.compositor.scale(),
                    seat.shell.session().active_theme(),
                );
            }
            self.desktop.offer_landing(None);
        }

        /// Have a save dropped on the bare desktop stand where it was let go,
        /// under the name the session's own open makes it as.
        fn land_save(&mut self, seat: &mut Seat<'_>, step: Option<&PickStep>) {
            let Some(PickStep::Open {
                pick, serial, path, ..
            }) = step
            else {
                return;
            };
            let folder = self.desktop.folder_path();
            let Some(name) = path
                .strip_prefix(folder.as_str())
                .and_then(|rest| rest.strip_prefix('/'))
                .filter(|name| !name.contains('/'))
            else {
                return;
            };
            let layout = seat.shell.desktop_layout(seat.compositor, self.desktop);
            self.desktop.expect_save(
                (pick.get(), *serial),
                name,
                seat.shell.router().pointer(),
                (
                    &layout,
                    seat.compositor.scale(),
                    seat.shell.session().active_theme(),
                ),
            );
        }

        /// What the application on icon-bar slot `slot` answers a drag of
        /// `items` with: it opens the one file, or it refuses, said in its
        /// name; `None` for a slot no installed bundle is attested to.
        fn slot_answer(&self, slot: usize, items: &DragItems) -> Option<SlotAnswer> {
            // Only a slot attested to an installed bundle can be vouched for,
            // and only a bundle whose signed manifest claims the file takes
            // it — by the one matching rule "Open With" uses.
            let bundle = self.apps.strip.get(slot)?.bundle.as_deref()?;
            let association = self.programs.association(bundle)?;
            let name = items.first().as_str();
            let claimed = items.openable()
                && !tairix_browse::applications_for(
                    name,
                    items.filetype(),
                    core::slice::from_ref(association),
                )
                .is_empty();
            if !claimed {
                return Some(SlotAnswer::Refuses(alloc::boxed::Box::new(
                    DropRefusal::new(
                        association.name(),
                        name,
                        items.openable(),
                        association.mime_types(),
                    ),
                )));
            }
            Some(SlotAnswer::Opens(alloc::boxed::Box::new(DropTarget {
                run_path: BundleRunPath::new(&tairix_appstore::entry_path(bundle)).ok()?,
                writes_documents: association.writes_documents(),
            })))
        }

        /// Tell the window icons carried off the desktop were last offered to
        /// that the offer ended — dropped there, its application moves or
        /// copies them itself — and give the desktop its lifted icons back:
        /// put down where they were let go on it, else where they stood.
        fn settle_desktop(&mut self, seat: &mut Seat<'_>, end: DesktopEnd, now_ns: u64) {
            if let Some(offer) = end.offer {
                deliver(
                    self.server,
                    self.sink,
                    seat.shell,
                    seat.compositor,
                    self.windows,
                    self.picker,
                    &mut self.apps.service,
                    seat.menu,
                    &offer.event(),
                );
            }
            let layout = seat.shell.desktop_layout(seat.compositor, self.desktop);
            let mut damage = Region::new();
            let at = match end.dropped {
                DesktopDrop::Desktop(at) => at,
                dropped => {
                    // A placed file is read into the window's document, so its
                    // icon stays where it stood.
                    self.desktop.restore_lent(&layout, &mut damage);
                    seat.shell
                        .present_desktop_area(seat.compositor, self.desktop, &damage);
                    if let DesktopDrop::Placed(placed) = dropped {
                        ask_for_desktop_call(
                            DesktopCall::Place(placed),
                            self.files,
                            self.desktop,
                            seat.shell,
                            seat.compositor,
                            &mut LaunchCtx {
                                launched: self.launched,
                                apps: &self.apps.service,
                                server: self.server,
                                sink: self.sink,
                                windows: self.windows,
                                identity: self.identity,
                            },
                        );
                    }
                    return;
                }
            };
            let outcome = self.desktop.put_down_lent(at, &layout, &mut damage);
            let applied = apply_desktop_action(
                outcome.action,
                self.publisher,
                self.files,
                self.pinboard,
                self.wallpapers,
                self.desktop,
                seat.shell,
                seat.compositor,
                &mut LaunchCtx {
                    launched: self.launched,
                    apps: &self.apps.service,
                    server: self.server,
                    sink: self.sink,
                    windows: self.windows,
                    identity: self.identity,
                },
                now_ns,
            );
            let refused =
                show_desktop_change(applied, &damage, self.desktop, seat.shell, seat.compositor);
            if let Some(refusal) = refused {
                state_refusal(&refusal, self.windows, seat.shell, seat.compositor);
            }
        }

        /// Carry icons pressed on the desktop on over the application window
        /// the pointer reached, where the desktop's own layer would draw them
        /// out of sight beneath it: the seat takes them above everything,
        /// lifted off the desktop until the drag ends, and offers them to the
        /// window at once, so a release that follows with no further motion is
        /// already answered for. Let go over a window in the batch that reached
        /// it, they stand where they rested, since it was never offered them.
        fn carry_desktop_over_window(&mut self, seat: &mut Seat<'_>) {
            let pointer = seat.shell.router().pointer();
            let scale = seat.compositor.scale();
            let over_window = self.desktop.carries_to(pointer, scale)
                && seat
                    .compositor
                    .window_at(pointer)
                    .and_then(|wm| self.windows.ipc_id(wm))
                    .is_some();
            if !over_window {
                return;
            }
            let layout = seat.shell.desktop_layout(seat.compositor, self.desktop);
            let mut damage = Region::new();
            let held = seat.shell.router().holds_primary();
            let mut handed = false;
            if held {
                let theme = seat.shell.session().active_theme();
                let _ =
                    self.desktop
                        .follow_press(pointer, true, &layout, scale, theme, &mut damage);
                if let Some(cargo) = self.desktop.carried_cargo(pointer, &layout) {
                    handed = seat
                        .shell
                        .begin_desktop_drag(seat.compositor, cargo)
                        .is_ok();
                }
            }
            if handed {
                self.desktop.lend_carry(&layout, &mut damage);
            } else if !held {
                self.desktop.cancel_carry(&layout, &mut damage);
            }
            if !damage.is_empty() {
                seat.shell
                    .present_desktop_area(seat.compositor, self.desktop, &damage);
            }
            if handed {
                let place = self.drag_place(seat, pointer);
                let surface = self.drag_surface(seat, place);
                seat.shell.drag_over(seat.compositor, surface);
                if let Some(report) = seat.shell.take_drag_report() {
                    self.report_drag(seat, report);
                }
            }
        }

        /// Tell the window a carried save was last offered to that it ended,
        /// and save the document where it was dropped.
        fn settle_save(&mut self, seat: &mut Seat<'_>, end: SaveEnd) {
            // The wait is recorded before the window hears of the drop, so a
            // delivery that finds it gone ends the wait rather than missing it.
            let step = self
                .picker
                .save_dropped(end.pick, end.dropped, seat.shell, seat.compositor);
            self.land_save(seat, step.as_ref());
            if let Some(mut offer) = end.offer {
                // A drop the panel refused asks the window for no folder.
                let awaited = self
                    .picker
                    .awaits_drop(end.pick, offer.window_id, offer.serial);
                if !awaited {
                    offer.dropped = None;
                }
                let delivered = deliver(
                    self.server,
                    self.sink,
                    seat.shell,
                    seat.compositor,
                    self.windows,
                    self.picker,
                    &mut self.apps.service,
                    seat.menu,
                    &offer.event(),
                );
                if awaited && !delivered {
                    self.picker
                        .drop_unreached(end.pick, seat.shell, seat.compositor);
                }
            }
            step_pick(
                step,
                &Openers {
                    identity: self.identity,
                    programs: self.programs,
                },
                self.files,
                self.server,
                self.sink,
                seat.shell,
                seat.compositor,
                self.windows,
                self.picker,
                self.apps,
                seat.menu,
            );
            light_drop_icon(seat.shell, seat.compositor, self.desktop);
        }

        /// The compositor window `wm` under a carried save or icons at `at`: an
        /// offer to an application's window there, and nothing on the
        /// desktop's own surfaces, the window `requester` a save is for, a
        /// window an alert holds, or a window's furniture.
        fn offer_window(
            &self,
            compositor: &Compositor,
            requester: Option<u64>,
            wm: tairix_wm::WindowId,
            at: tairix_wm::Point,
        ) -> DragPlace {
            let Some(window_id) = self.windows.ipc_id(wm).filter(|id| Some(*id) != requester)
            else {
                return DragPlace::Nothing;
            };
            match content_point(compositor, wm, at) {
                Some((x, y)) if !self.windows.is_held(window_id) => {
                    DragPlace::Offer { window_id, x, y }
                }
                _ => DragPlace::Nothing,
            }
        }

        /// The compositor window `wm` under a carried drag at `at`: the drag's
        /// own application's window, at a point in its content, which that
        /// application answers; another application's, which takes the drop
        /// as that application's slot does; or nothing a drop is offered to —
        /// the session's own surfaces, or a window's frame.
        fn drag_window(
            &self,
            compositor: &Compositor,
            source: u64,
            wm: tairix_wm::WindowId,
            at: tairix_wm::Point,
        ) -> DragPlace {
            let Some(window_id) = self.windows.ipc_id(wm) else {
                return DragPlace::Nothing;
            };
            let Some(local) = content_point(compositor, wm, at) else {
                return DragPlace::Nothing;
            };
            let (Some(source_owner), Some(owner)) = (
                self.server.owner_of(source),
                self.server.owner_of(window_id),
            ) else {
                return DragPlace::Nothing;
            };
            let under = if owner == source_owner {
                WindowUnder::Own(Some(local))
            } else {
                WindowUnder::Foreign(
                    self.apps
                        .strip
                        .iter()
                        .position(|group| group.owner == owner),
                    local,
                )
            };
            let held = self.windows.is_held(window_id);
            DragPlace::over_window(window_id, held, under)
        }
    }

    /// Where screen point `at` lies in compositor window `wm`'s content, or
    /// `None` where it lies outside it.
    fn content_point(
        compositor: &Compositor,
        wm: tairix_wm::WindowId,
        at: tairix_wm::Point,
    ) -> Option<(u32, u32)> {
        let client = compositor
            .window_client_rect(wm)
            .filter(|rect| rect.contains(at))?;
        let x = u32::try_from(i64::from(at.x) - i64::from(client.left())).ok()?;
        let y = u32::try_from(i64::from(at.y) - i64::from(client.top())).ok()?;
        Some((x, y))
    }

    /// Light the desktop folder icon a carried drag would drop into while its
    /// application accepts it there, and darken the one it no longer would,
    /// painting only those cells.
    fn light_drop_icon<S: DirectorySource>(
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        desktop: &mut Desktop<S>,
    ) {
        let layout = shell.desktop_layout(compositor, desktop);
        let mut damage = Region::new();
        desktop.set_drop_target(shell.drag_drop_icon(), &layout, &mut damage);
        if !damage.is_empty() {
            shell.present_desktop_area(compositor, desktop, &damage);
        }
    }

    /// Hand the desktop each step its drops took since the last turn, holding
    /// back and releasing the artwork each names, and repaint the icons they
    /// moved.
    fn adopt_landings<S: DirectorySource>(
        steps: Vec<(u64, u32, LandingStep)>,
        desktop: &mut Desktop<S>,
        artworks: &Artworks,
        (shell, compositor): (&mut DesktopShell, &mut Compositor),
    ) {
        if steps.is_empty() {
            return;
        }
        let mut cells = damage::sink();
        for (window_id, serial, step) in steps {
            let work = desktop.step_landing(
                (window_id, serial),
                &step,
                |desk| shell.desktop_layout(compositor, desk),
                &mut cells,
            );
            artworks.adopt_landing(&work);
        }
        if !cells.is_empty() {
            shell.present_desktop_area(compositor, desktop, &cells);
        }
    }

    /// Stand the file the open `attempt` made at `made` where its save was let
    /// go on the desktop, repainting the icons that moved.
    fn land_saved<S: DirectorySource>(
        desktop: &mut Desktop<S>,
        attempt: (u64, u64),
        made: Option<&str>,
        (shell, compositor): (&mut DesktopShell, &mut Compositor),
    ) {
        let mut cells = damage::sink();
        let layout = |desk: &Desktop<S>| shell.desktop_layout(compositor, desk);
        if desktop.saved(attempt, made, layout, &mut cells) {
            shell.present_desktop_area(compositor, desktop, &cells);
        }
    }

    /// Load the user's pinboard settings into `desktop` (reporting an
    /// unusable store loudly) and answer with the session's pinboard state
    /// over the production file seams and `sandbox`. `publisher` publishes
    /// over what was loaded only once the store has been read.
    ///
    /// The settings are applied to the model here rather than returned, so
    /// only the desktop ever holds what is in force, and the appearance the
    /// stored document asks for is put into effect before the first frame —
    /// a desktop that came up in the default appearance because nothing read
    /// its own `appearance` key would be showing a setting the user did not
    /// choose.
    fn load_pinboard<S: DirectorySource>(
        desktop: &mut Desktop<S>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        publisher: &Publisher,
        sandbox: SharedSandbox,
    ) -> PinboardPanel {
        let loaded = read_pinboard_store(&mut RtHost);
        for warning in &loaded.warnings {
            app::report_line(warning);
        }
        if loaded.read {
            publisher.read_store();
        }
        shell.adopt_files(loaded.files, first_settings_generation());
        desktop.hide_endings(loaded.files.presentation.endings);
        desktop.show_hidden_items(loaded.files.presentation.hidden_items);
        desktop.show_drives(loaded.files.desktop);
        let wanted = loaded.settings.clone();
        if let Some(change) = desktop.apply_settings(loaded.settings) {
            // Through the same adopt path a later change takes, so the
            // desktop a user logs in to and the desktop they get from
            // changing a setting are drawn by one piece of code.
            adopt_appearance(change.appearance, &wanted, shell, compositor);
        }
        PinboardPanel {
            sandbox,
            prepared: None,
        }
    }

    /// Where this session starts counting its publications of the file
    /// manager's settings: drawn, because every session on the machine
    /// publishes the one `Desktop` notice the count rides, and counts from
    /// zero would meet.
    fn first_settings_generation() -> u32 {
        let mut drawn = [0u8; 4];
        if let Err(errno) = tairix_rt::random_fill(&mut drawn) {
            app::report(
                APP_NAME,
                format_args!(
                    "no random seed to be had ({errno}); the clock starts the settings \
                     count"
                ),
            );
            let [a, b, c, d, ..] = tairix_rt::clock_get().to_le_bytes();
            drawn = [a, b, c, d];
        }
        u32::from_le_bytes(drawn)
    }

    /// Ask for the wallpaper the desktop layer should be painted over, and
    /// install it when it is ready.
    ///
    /// Called both when something changed (bring-up, a settings apply, a resume
    /// at a new mode) and on the wake that says a preparation finished, because
    /// the two are the same question: *is what the desktop wants what is
    /// installed?* Answering it costs a comparison when nothing changed, so a
    /// wake the desktop has already acted on is almost free.
    ///
    /// Reads a file and runs a sandboxed decode, so it happens on a worker
    /// thread; the desktop keeps painting whatever it has until the answer
    /// lands, and a wallpaper that cannot be read or rendered installs no
    /// surface and leaves the backdrop colour showing (stated once, by the
    /// worker that observed it). Answers whether the desktop layer needs
    /// repainting.
    fn prepare_wallpaper<S: DirectorySource>(
        pinboard: &mut PinboardPanel,
        wallpapers: &Wallpapers,
        shell: &mut DesktopShell,
        desktop: &Desktop<S>,
        compositor: &Compositor,
        now_ns: u64,
    ) -> bool {
        let wanted = WallpaperSource::wanted(desktop.settings(), compositor.screen_rect());
        if pinboard.prepared.as_ref() == Some(&wanted) {
            return false;
        }
        match wallpapers.request(&wanted, &pinboard.sandbox) {
            Prepared::Pending => false,
            Prepared::Ready { surface, refusal } => {
                // Stated here, on the serve loop's own thread, so a worker's
                // diagnosis cannot interleave with anything else reaching
                // `stderr`.
                if let Some(reason) = refusal {
                    app::report(APP_NAME, reason);
                }
                pinboard.prepared = Some(wanted);
                shell.set_wallpaper(surface, desktop.settings().backdrop, now_ns);
                true
            }
        }
    }

    /// Deliver every answer the chain owes, and bring the screen into line
    /// with what it now has.
    ///
    /// The session's **one** delivery point. Every close — a chosen row, a
    /// dismissal, a chain displaced by the next open, an owner's death, a
    /// mode change — queues its answer here rather than sending its own, so
    /// no chain can be answered twice and none can be left unanswered.
    #[allow(clippy::too_many_arguments)] // The chain's whole mutable surround, threaded explicitly.
    fn answer_menu_chain<S: DirectorySource, F: PickSources<S>>(
        menu: &mut MenuChain,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        windows: &mut SessionWindows,
        server: &mut WindowServer<RtShmMapper>,
        sink: &mut RtEventSink,
        picker: &mut SessionPicker<S, F>,
        apps: &mut dyn AppBarBridge,
        identity: &RtWindowIdentity,
        desk: &mut DesktopMenuDesk<'_, S>,
        now_ns: u64,
    ) {
        // Before the drain, not after: a chain the mode has moved under owes
        // an answer of its own, and settling once the queue is empty would
        // leave it sitting there until the next event.
        {
            let geom = chain_geometry(shell.session(), compositor);
            menu.settle_mode(&geom);
        }
        for (owner, outcome) in menu.take_answers() {
            // A total match, so a third kind of owner cannot silently be
            // answered as the desktop's own.
            let (window_id, open_id) = match owner {
                ChainOwner::Window { window_id, open_id } => (window_id, open_id),
                ChainOwner::Backdrop => {
                    let refused = answer_backdrop_menu(
                        &outcome,
                        shell,
                        compositor,
                        desk,
                        &mut LaunchReach {
                            apps,
                            server,
                            sink,
                            windows,
                            identity,
                        },
                        now_ns,
                    );
                    if let Some(refusal) = refused {
                        state_refusal(&refusal, windows, shell, compositor);
                    }
                    continue;
                }
                ChainOwner::Bar(subject) => {
                    answer_bar_menu(
                        &subject,
                        &outcome,
                        (shell, compositor.scale(), now_ns),
                        desk,
                    );
                    continue;
                }
                ChainOwner::Picker { pick, .. } => {
                    picker.menu_answered(pick, &outcome, shell, compositor);
                    continue;
                }
                ChainOwner::Capture => {
                    desk.capture.menu_answered(&outcome);
                    continue;
                }
            };
            let outcome = match outcome {
                ChainOutcome::Chosen(item) => MenuOutcome::Chosen(item),
                // The text is wider than the one fixed event frame, so the
                // answer names the field and the engine holds the text for
                // the application to pull. Recorded *before* the answer goes
                // out, so a pull cannot arrive ahead of what it asks for; a
                // refusal to record leaves the gesture a dismissal rather
                // than an answer naming a text nobody holds.
                ChainOutcome::Entered(entry, text) => {
                    match server.record_menu_text(window_id, open_id, &text) {
                        Ok(()) => MenuOutcome::Entered(entry),
                        Err(err) => {
                            app::report(APP_NAME, format_args!("menu text refused ({err:?})"));
                            MenuOutcome::Dismissed
                        }
                    }
                }
                ChainOutcome::Dismissed => MenuOutcome::Dismissed,
                ChainOutcome::Refused(reason) => MenuOutcome::Refused(reason),
            };
            deliver(
                server,
                sink,
                shell,
                compositor,
                windows,
                picker,
                apps,
                menu,
                &WindowEvent::MenuClosed {
                    window_id,
                    open_id,
                    outcome,
                },
            );
        }
        present_menu_chain(menu, shell, compositor, windows);
    }

    /// Answer one of the **icon bar's** own chains in process: read the chosen
    /// row back through the bar's own subject and queue what it asks for.
    ///
    /// The bar keeps the vocabulary, so this asks it rather than interpreting
    /// an id here, and the answer joins the outcomes a click on the bar
    /// produces. A row id the menu never declared names nothing and is dropped
    /// (fail closed — never guessed at); a refusal is stated and the bar
    /// carries on.
    fn answer_bar_menu<S: DirectorySource>(
        subject: &MenuSubject,
        outcome: &ChainOutcome,
        (shell, scale, now_ns): (&mut DesktopShell, tairix_wm::Scale, u64),
        desk: &mut DesktopMenuDesk<'_, S>,
    ) {
        let taskbar = shell.session_mut().taskbar_mut();
        let response = match outcome {
            ChainOutcome::Chosen(item) => taskbar.menu_chosen(subject, *item, now_ns, scale),
            // Only an application's own menu declares a quick-entry field.
            ChainOutcome::Entered(item, text) => subject.entered(*item, text.clone()),
            ChainOutcome::Dismissed => return,
            ChainOutcome::Refused(reason) => {
                app::report(APP_NAME, format_args!("no bar menu ({reason:?})"));
                return;
            }
        };
        if let Some(response) = response {
            desk.answered
                .push(tairix_desktop_session::ShellOutcome::Taskbar(response));
        }
    }

    /// Open one of the icon bar's own menus as the seat's one chain.
    ///
    /// The bar hands over a model, an anchor and which menu it is; everything
    /// after that — titling, placement, drawing, the grab, traversal,
    /// dismissal, and the one answer — is the chain's, exactly as for an
    /// application's `OpenMenu`. A refused menu is an answer stated on
    /// `stderr`, never a reason to draw one on the bar.
    fn open_bar_menu(
        request: MenuRequest,
        seat_modal: bool,
        menu: &mut MenuChain,
        (shell, compositor): (&mut DesktopShell, &mut Compositor),
        windows: &SessionWindows,
        apps: &AppBarService,
    ) {
        let mut model = request.model;
        if let MenuSubject::App { owner, .. } = request.subject {
            if let Some(bundle) = apps.attested_bundle(owner) {
                model.set_info_source(&RowIcon::Bundle(alloc::string::String::from(bundle)));
            }
        }
        let geom = chain_geometry(shell.session(), compositor);
        match open_desktop_menu(
            menu,
            ChainOwner::Bar(request.subject),
            model,
            request.placement,
            seat_modal,
            &geom,
        ) {
            Ok(()) => present_menu_chain(menu, shell, compositor, windows),
            Err(refused) => {
                app::report(APP_NAME, format_args!("no bar menu ({refused:?})"));
            }
        }
    }

    /// Answer the desktop's own chain in process: put a chosen row's command,
    /// or the title a drive's *Title* field committed, through the desktop
    /// model and the one action path.
    ///
    /// The model resolves the command against its own state, so a row and the
    /// equivalent gesture on the icon column produce the very same action; the
    /// session merely carries it out. A row id the menu never declared names
    /// no command and is dropped (fail closed — never guessed at).
    fn answer_backdrop_menu<S: DirectorySource>(
        outcome: &ChainOutcome,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        desk: &mut DesktopMenuDesk<'_, S>,
        reach: &mut LaunchReach<'_>,
        now_ns: u64,
    ) -> Option<GestureRefusal> {
        let (acted, refresh) = match outcome {
            ChainOutcome::Chosen(item) => {
                let command = PinboardCommand::from_item(*item)?;
                let layout = shell.desktop_layout(compositor, desk.desktop);
                let acted = desk
                    .desktop
                    .command(command, &layout, &desk.programs.associations);
                (acted, command == PinboardCommand::Refresh)
            }
            // A drive's *Title* field is the menu's one field.
            ChainOutcome::Entered(item, text)
                if PinboardCommand::from_item(*item) == Some(PinboardCommand::Title) =>
            {
                (desk.desktop.retitle_selection(text), false)
            }
            ChainOutcome::Entered(..) | ChainOutcome::Dismissed => return None,
            ChainOutcome::Refused(reason) => {
                app::report(APP_NAME, format_args!("no backdrop menu ({reason:?})"));
                return None;
            }
        };
        let applied = apply_desktop_action(
            acted.action,
            desk.publisher,
            desk.files,
            desk.pinboard,
            desk.wallpapers,
            desk.desktop,
            shell,
            compositor,
            &mut LaunchCtx {
                launched: desk.launched,
                apps: reach.apps,
                server: reach.server,
                sink: reach.sink,
                windows: reach.windows,
                identity: reach.identity,
            },
            now_ns,
        );
        // Asking to look again is the honest moment to re-read what is
        // installed too, so a program installed since bring-up can open a
        // document from here without the library popup being opened first.
        if refresh {
            request_programs(desk.catalogs, shell, compositor, desk.programs);
        }
        if acted.rearranged || applied == Ok(true) {
            shell.present_desktop(compositor, desk.desktop);
        }
        applied.err()
    }

    /// Reconcile the compositor against the chain, resolving the owner's and
    /// the attached window's compositor windows the session holds.
    /// Serve what a pointer at rest is owed and no event says: a tip whose
    /// dwell has elapsed, and a crossed menu row rested on until it chose.
    /// Run ahead of both of the loop's presents, so a client that keeps the
    /// loop busy holds neither off and the frame carries them.
    fn serve_rest(
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        menu: &mut MenuChain,
        windows: &SessionWindows,
        now_ns: u64,
    ) {
        if shell.tooltip_tick(now_ns) {
            shell.present_tooltip(compositor);
        }
        if shell.tick_menu_chain(compositor, menu, now_ns) {
            present_menu_chain(menu, shell, compositor, windows);
        }
    }

    fn present_menu_chain(
        menu: &mut MenuChain,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        windows: &SessionWindows,
    ) {
        let owner = menu.owner_window().and_then(|id| windows.wm_id(id));
        if !shell.present_menu_chain(compositor, menu, owner) && menu.exhausted() {
            // The chain could not be drawn, so it is refused rather than left
            // half on the screen; taking it down needs the reconcile to run
            // once more, now over an empty list.
            let _ = shell.present_menu_chain(compositor, menu, owner);
        }
        // The chain's own presentation declared or withdrew the hovered row's
        // explanation; this raises the plate above the surfaces it explains.
        shell.present_tooltip(compositor);
    }

    /// Attest the caller of a pending pinboard call from the kernel, lay the
    /// settings it carries over the ones in effect through the shared,
    /// host-tested policy, and adopt the result through the session's one
    /// persist-then-adopt path.
    ///
    /// Only a caller running as this session's own user may rewrite this
    /// session's desktop: the uid compared is the kernel-attested
    /// `call_peer_origin` uid, never a wire claim. Anything else — another
    /// user's process, a malformed frame, an unusable document, a refused
    /// store write — is a typed refusal stated on `stderr` that adopts
    /// nothing (fail closed). The document merely *names* a wallpaper path;
    /// the session reads it under its own identity, so this channel reaches
    /// no file the session could not already read.
    #[allow(clippy::too_many_arguments)] // The desktop's whole mutable state, threaded explicitly.
    fn apply_pinboard<S: DirectorySource>(
        applied: PinboardApplied,
        publisher: &Publisher,
        pinboard: &mut PinboardPanel,
        wallpapers: &Wallpapers,
        desktop: &mut Desktop<S>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        covered: bool,
        ticket: u64,
    ) -> Option<TrialAsk> {
        let output = compositor.mode();
        match resolution_ask(
            applied.settings.resolution,
            desktop.settings().resolution,
            (output.width_px, output.height_px),
            covered,
        ) {
            ResolutionAsk::Same => {}
            ResolutionAsk::Trial(size) => {
                return Some(TrialAsk {
                    size,
                    trial: Trial {
                        request: alloc::string::String::from(applied.document.as_str()),
                        ticket,
                    },
                });
            }
            ResolutionAsk::Covered => {
                reply_pinboard(ticket, Err(Errno::Busy));
                return None;
            }
        }
        // The call is answered when the store has spoken, not now, so the
        // chooser still learns whether its document was actually published —
        // and this loop does not wait to find out.
        if request_pinboard_settings(
            applied.settings,
            publisher,
            Some(ticket),
            pinboard,
            wallpapers,
            desktop,
            shell,
            compositor,
            tairix_rt::clock_get(),
        ) {
            shell.present_desktop(compositor, desktop);
        }
        None
    }

    /// Attest a pending pinboard call against the session's own identity,
    /// merging the settings an apply carries over the ones in effect.
    ///
    /// # Errors
    ///
    /// The wire [`Errno`] for a caller the kernel attests as another user, a
    /// frame that will not decode, or a document this build's registry refuses
    /// — each stated on `stderr` in the session's own wording.
    fn attest_pinboard(
        session_uid: u32,
        held: (&DesktopSettings, &FilerPreferences, Option<&ZoneInfo<'_>>),
        ticket: u64,
        request: &[u8],
    ) -> Result<PinboardCall, Errno> {
        let origin = tairix_rt::peer_origin(PINBOARD_ENDPOINT, ticket)?;
        serve_pinboard(&LOG_SINK, (session_uid, origin.uid()), held, request).map_err(|refusal| {
            let msg = refusal.reason();
            app::report(APP_NAME, format_args!("{msg}"));
            refusal.errno()
        })
    }

    /// Serve the attested request `call` about the user's sound choices,
    /// answering `ticket` now or once its work has come back.
    #[allow(clippy::too_many_arguments)] // The loop state the requests reach, threaded explicitly.
    fn serve_sound_call<S: DirectorySource, F: PickSources<S>>(
        call: &SoundCall,
        ticket: u64,
        (effects, memory): (&mut Effects, &mut PrivateRecords),
        worker: &PrivateWorker,
        themes: &[alloc::string::String],
        picker: &mut SessionPicker<S, F>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        let now = tairix_rt::clock_get();
        let work = match *call {
            SoundCall::Read if !effects.admit_call() => Err(Errno::Busy),
            SoundCall::Read => match effects.read_ask() {
                Ok(ask) => {
                    match worker.submit(PrivateJob::Read { ask, ticket }) {
                        Ok(true) => pump_private(effects, memory, worker, now),
                        Ok(false) => {}
                        Err(_) => {
                            effects.release_call();
                            reply_pinboard(ticket, Err(Errno::Busy));
                        }
                    }
                    return;
                }
                Err(err) => {
                    effects.release_call();
                    Err(err)
                }
            },
            SoundCall::Themes { from } => {
                answer_themes(themes, from, ticket);
                return;
            }
            SoundCall::Apply(document) => {
                effects
                    .apply(document.as_str(), ticket)
                    .map_err(|(err, refusal)| {
                        if let Some(refusal) = refusal {
                            app::report(
                                APP_NAME,
                                format_args!("a sound settings document was refused: {refusal}"),
                            );
                        }
                        err
                    })
            }
            SoundCall::UseTheme(event) => effects.use_theme(event, ticket),
            SoundCall::Pick(event) => {
                let begun = effects.begin_pick(event, ticket).and_then(|()| {
                    picker
                        .begin_for(
                            Requester::Sound(event),
                            &tairix_abi::window_ipc::PickPurpose::Open,
                            shell,
                            compositor,
                        )
                        .inspect_err(|_| {
                            // Answered below, by the ticket it came with.
                            let _ = effects.abandon_pick();
                        })
                });
                if let Err(err) = begun {
                    reply_pinboard(ticket, Err(err));
                }
                return;
            }
        };
        match work {
            Ok(work) => submit_effects(effects, memory, worker, work, now),
            Err(err) => reply_pinboard(ticket, Err(err)),
        }
    }

    /// Re-resolve the icon bar's application strip from live state and push
    /// it to the bar.
    ///
    /// The strip is derived, never stored: every live served window is
    /// grouped under the process the window engine attested owns it, each
    /// process's bundle is the installed one the kernel attests it runs (never
    /// anything an application sent), and every application that declared a
    /// presence keeps a slot whether it owns a window or not. Slot icons are
    /// rasterised at the strip's own geometry through the shell's sandboxed
    /// pipeline, served from its one cache on every later push.
    fn refresh_app_strip(
        apps: &mut AppBarPanel,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        server: &WindowServer<RtShmMapper>,
        windows: &SessionWindows,
        identity: &RtWindowIdentity,
        bundles: &BundleIndex,
    ) {
        let owners = window_owners(shell, server, windows);
        // Attested rather than looked up in what the desktop launched: a
        // viewer the file manager spawned, or a program started from a shell,
        // is the same application either way.
        let strip = apps.service.strip(
            &owners,
            |owner| {
                identity
                    .app_of(owner)
                    .and_then(|app| bundles.path_of(&app))
                    .map(alloc::string::String::from)
            },
            bundles,
        );
        // What runs moves only with the bundles, not with a slot's artwork.
        apps.moved |= !strip
            .iter()
            .map(|group| group.bundle.as_deref())
            .eq(apps.strip.iter().map(|group| group.bundle.as_deref()));
        apps.strip = strip;
        let side = shell.session().taskbar().app_icon_side(compositor.scale());
        let slots = {
            let strip = core::mem::take(&mut apps.strip);
            let (cache, resolver) = shell.artwork_parts();
            let slots = apps.service.slots(&strip, (resolver, cache, side));
            apps.strip = strip;
            slots
        };
        shell.set_apps(compositor, slots);
        // The strip it now holds names the applications whose icons it will
        // draw, so anything not decoded yet is asked for before the next paint
        // rather than by it.
        shell.warm_icon_artwork(compositor);
    }

    /// Every live served window as `(attested owner, task)`, in the order
    /// the windows opened.
    ///
    /// The window-channel ids the engine mints rise with each open, so
    /// walking them in id order is walking the windows in the order they
    /// opened. A window the taskbar does not track as a task (a popup) has
    /// nothing to group.
    fn window_owners(
        shell: &DesktopShell,
        server: &WindowServer<RtShmMapper>,
        windows: &SessionWindows,
    ) -> alloc::vec::Vec<(tairix_abi::ProcId, TaskId)> {
        windows
            .served()
            .filter_map(|(ipc, wm)| Some((server.owner_of(ipc)?, shell.tasks().task_for(wm)?)))
            .collect()
    }

    /// Whether the strip the bar shows still describes the live windows.
    ///
    /// Pure in-memory bookkeeping — no manifest and no icon is re-read — so
    /// it is cheap enough to run once per wake, and the strip is re-pushed
    /// only when a window actually opened, closed, or changed hands.
    fn app_strip_is_stale(
        apps: &AppBarPanel,
        shell: &DesktopShell,
        server: &WindowServer<RtShmMapper>,
        windows: &SessionWindows,
    ) -> bool {
        let held: alloc::vec::Vec<(tairix_abi::ProcId, TaskId)> = apps
            .strip
            .iter()
            .flat_map(|group| group.windows.iter().map(move |&task| (group.owner, task)))
            .collect();
        // A window whose application the bar deliberately shows no slot for
        // is absent from the strip by design, not because the strip aged.
        let mut sorted: alloc::vec::Vec<(tairix_abi::ProcId, TaskId)> =
            window_owners(shell, server, windows)
                .into_iter()
                .filter(|&(owner, _)| !apps.service.is_iconless(owner))
                .collect();
        sorted.sort_unstable();
        let mut held = held;
        held.sort_unstable();
        sorted != held
    }

    /// Ask the authority to record this session as background and, only on
    /// its acceptance, give the screen up through `screen`.
    ///
    /// Answers whether the session is now background. A refusal — the
    /// authority said no, could not be reached, or answered something that
    /// is not a verdict — is stated to the user and changes nothing: the
    /// desktop keeps the seat and keeps drawing.
    fn step_aside(switch: &mut SwitchUser, screen: SessionDeparture<'_>) -> bool {
        let mut screen = screen;
        match switch.step_aside(&mut RtSessionAuthority, &mut screen) {
            Ok(()) => true,
            Err(refusal) => {
                app::report(APP_NAME, refusal.reason());
                false
            }
        }
    }

    /// Route one shell outcome onward: mirror focus changes and pointer
    /// presses to the owning app over the window channel, hand the raw
    /// key record to the focused served window (or the focused picker,
    /// or the showing confirmation prompt), and spawn the launcher
    /// selection. Everything else is complete inside the shell.
    #[allow(clippy::too_many_arguments)] // The serve loop's whole mutable state, threaded explicitly.
    #[allow(clippy::too_many_lines)] // One linear match over every outcome; splitting it would hide the routing policy.
    fn route_outcome<S: DirectorySource, F: PickSources<S>>(
        outcome: Timed<tairix_desktop_session::ShellOutcome>,
        key: Option<KeyInput>,
        catalogs: &Catalogs,
        (files, desktop): (&Listings, &mut Desktop<S>),
        focused: &mut Option<u64>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        windows: &mut SessionWindows,
        server: &mut WindowServer<RtShmMapper>,
        sink: &mut RtEventSink,
        picker: &mut SessionPicker<S, F>,
        (confirm, (elevate, elevator), resolution): (
            &mut ConfirmPrompt,
            (&mut AuthorisePrompt, &mut RtElevator),
            &mut ResolutionPrompt,
        ),
        lock: &mut ScreenLock,
        menu: &mut MenuChain,
        account: &str,
        shown_name: &str,
        identity: &RtWindowIdentity,
        launched: &mut LaunchTable,
        apps: &mut AppBarPanel,
        switchboard: &mut Option<u64>,
        pending_open: &mut Option<CommandSection>,
        programs: &mut Programs,
        system: &SystemFacts,
        memory: &mut LauncherMemory,
    ) -> Routed {
        use tairix_desktop_session::ShellOutcome;
        let Timed {
            value: outcome,
            at_ns,
        } = outcome;
        match outcome {
            ShellOutcome::WindowManager(response) => match response {
                InputResponse::Activated { window, local }
                | InputResponse::SecondaryActivated { window, local }
                    if windows.alerts.owns(window) || windows.holder_of(window).is_some() =>
                {
                    // An alert takes the press, and so does a window an alert
                    // or a dialog holds: that press hands the keyboard to its
                    // holder and reaches no application.
                    let mut answer = None;
                    if let Some(holder) = windows.holder_of(window) {
                        shell.focus_holder(compositor, holder);
                    } else if matches!(response, InputResponse::Activated { .. }) {
                        answer = windows.alerts.pointer(
                            window,
                            local,
                            &tairix_wm::InputEvent::PointerPressed {
                                button: tairix_wm::PointerButton::Primary,
                            },
                            at_ns,
                            &mut windows.holds,
                            shell,
                            compositor,
                        );
                    }
                    reconcile_focus(
                        focused,
                        server,
                        sink,
                        shell,
                        compositor,
                        windows,
                        picker,
                        &mut apps.service,
                        menu,
                    );
                    if let Some(answer) = answer {
                        relay_alert_answer(answer, server, sink, shell, windows);
                    }
                }
                InputResponse::Activated { window, local } => {
                    let target = windows.ipc_id(window);
                    // The focus change reaches the apps before the press.
                    tell_focus(
                        focused,
                        target,
                        server,
                        sink,
                        shell,
                        compositor,
                        windows,
                        picker,
                        &mut apps.service,
                        menu,
                    );
                    // The activating press itself, window-local. A
                    // negative coordinate cannot occur for an in-window
                    // press; refuse rather than wrap if it ever did.
                    if let (Some(id), Ok(x), Ok(y)) =
                        (target, u32::try_from(local.x), u32::try_from(local.y))
                    {
                        deliver(
                            server,
                            sink,
                            shell,
                            compositor,
                            windows,
                            picker,
                            &mut apps.service,
                            menu,
                            &WindowEvent::Pointer {
                                window_id: id,
                                x,
                                y,
                                action: PointerAction::Pressed(
                                    tairix_abi::input::PointerButtonCode::Primary,
                                ),
                                modifiers: pointer_modifiers(shell),
                                at_ns,
                            },
                        );
                    }
                    // A press on a picker window navigates its pick: the
                    // row hit-test, descent, and choose rules are the shared
                    // engine's, and a concluded pick delegates (or cancels)
                    // below.
                    if let Some(pick) = picker.shown_in(window) {
                        let step = picker.handle_click(pick, local, at_ns, shell, compositor);
                        step_pick(
                            step,
                            &Openers { identity, programs },
                            files,
                            server,
                            sink,
                            shell,
                            compositor,
                            windows,
                            picker,
                            apps,
                            menu,
                        );
                    }
                    // A press on the picker's question whether to replace a
                    // file answers it, and a yes saves over the file.
                    if let Some(pick) = picker.asking_in(window) {
                        let step = picker.question_click(pick, local, at_ns, shell, compositor);
                        step_pick(
                            step,
                            &Openers { identity, programs },
                            files,
                            server,
                            sink,
                            shell,
                            compositor,
                            windows,
                            picker,
                            apps,
                            menu,
                        );
                    }
                    // A press on the showing confirmation prompt answers it:
                    // only the confirming button relays the transition, and
                    // the prompt window is already gone by then.
                    if confirm.wm_id() == Some(window) {
                        let answer = confirm.handle_click(local, at_ns, shell, compositor);
                        if let Some(answer) = answer {
                            report_power_relay(answer, *switchboard);
                        }
                    }
                    // A press on the resolution prompt answers it; the serve
                    // loop puts the answer into effect.
                    if resolution.wm_id() == Some(window) {
                        let _ = resolution.handle_click(local, at_ns, shell, compositor);
                    }
                }
                InputResponse::SecondaryActivated { window, local } => {
                    // A right-click raises+focuses the window like a primary
                    // press, then delivers a secondary-button press so the
                    // client can open its context menu. Mirror the focus change
                    // app-ward first (the old window unfocuses, the new one
                    // focuses), then deliver the press. The trusted picker is a
                    // read-only browser with no context menu, so a right-click
                    // on it delivers focus only and opens nothing.
                    let target = windows.ipc_id(window);
                    tell_focus(
                        focused,
                        target,
                        server,
                        sink,
                        shell,
                        compositor,
                        windows,
                        picker,
                        &mut apps.service,
                        menu,
                    );
                    if let (Some(id), Ok(x), Ok(y)) =
                        (target, u32::try_from(local.x), u32::try_from(local.y))
                    {
                        deliver(
                            server,
                            sink,
                            shell,
                            compositor,
                            windows,
                            picker,
                            &mut apps.service,
                            menu,
                            &WindowEvent::Pointer {
                                window_id: id,
                                x,
                                y,
                                action: PointerAction::Pressed(
                                    tairix_abi::input::PointerButtonCode::Secondary,
                                ),
                                modifiers: pointer_modifiers(shell),
                                at_ns,
                            },
                        );
                    }
                }
                // A press on the backdrop, primary or secondary, means the
                // desktop holds the keyboard: the window that had it learns
                // it lost it. The secondary press additionally opens the
                // backdrop menu, which `route_desktop` has already applied.
                InputResponse::DesktopPressed | InputResponse::DesktopSecondaryPressed => {
                    tell_focus(
                        focused,
                        None,
                        server,
                        sink,
                        shell,
                        compositor,
                        windows,
                        picker,
                        &mut apps.service,
                        menu,
                    );
                }
                InputResponse::Key { window, .. } if windows.alerts.owns(window) => {
                    if let Some(record) = key {
                        if let Some(answer) = windows.alerts.key(
                            window,
                            &record,
                            tairix_rt::clock_get(),
                            &mut windows.holds,
                            shell,
                            compositor,
                        ) {
                            reconcile_focus(
                                focused,
                                server,
                                sink,
                                shell,
                                compositor,
                                windows,
                                picker,
                                &mut apps.service,
                                menu,
                            );
                            relay_alert_answer(answer, server, sink, shell, windows);
                        }
                    }
                }
                InputResponse::Key { window, .. } => {
                    if let Some(pick) = picker.shown_in(window) {
                        // The focused picker consumes its own keys; a
                        // concluded pick delegates (or cancels) below and
                        // the key never reaches a served window.
                        if let Some(record) = key {
                            let now = tairix_rt::clock_get();
                            let step = picker.handle_key(pick, &record, now, shell, compositor);
                            step_pick(
                                step,
                                &Openers { identity, programs },
                                files,
                                server,
                                sink,
                                shell,
                                compositor,
                                windows,
                                picker,
                                apps,
                                menu,
                            );
                        }
                    } else if let Some(pick) = picker.asking_in(window) {
                        if let Some(record) = key {
                            let now = tairix_rt::clock_get();
                            let step = picker.question_key(pick, &record, now, shell, compositor);
                            step_pick(
                                step,
                                &Openers { identity, programs },
                                files,
                                server,
                                sink,
                                shell,
                                compositor,
                                windows,
                                picker,
                                apps,
                                menu,
                            );
                        }
                    } else if confirm.wm_id() == Some(window) {
                        // The focused prompt consumes its own keys the same
                        // way, so `Escape` declines and no key reaches an app
                        // while the question is unanswered.
                        if let Some(record) = key {
                            let now_ns = tairix_rt::clock_get();
                            let answer = confirm.handle_key(&record, now_ns, shell, compositor);
                            if let Some(answer) = answer {
                                report_power_relay(answer, *switchboard);
                            }
                        }
                    } else if resolution.wm_id() == Some(window) {
                        if let Some(record) = key {
                            let now_ns = tairix_rt::clock_get();
                            let _ = resolution.handle_key(&record, now_ns, shell, compositor);
                        }
                    } else if let (Some(id), Some(record)) = (windows.ipc_id(window), key) {
                        deliver(
                            server,
                            sink,
                            shell,
                            compositor,
                            windows,
                            picker,
                            &mut apps.service,
                            menu,
                            &WindowEvent::Key {
                                window_id: id,
                                key: record,
                            },
                        );
                    }
                }
                // A wheel gesture over a window that owns its own content
                // scrolling (no window-manager root viewport): the turn
                // belongs to the application, so forward it to that window's
                // owner over the window channel. The picker is the session's
                // own window, so a turn over it scrolls its listing here.
                InputResponse::AppScroll {
                    window,
                    local,
                    dx,
                    dy,
                    modifiers,
                } => {
                    if let Some(pick) = picker.shown_in(window) {
                        picker.scroll(pick, (dx, dy), shell, compositor);
                    } else if windows.alerts.owns(window) {
                        windows.alerts.scroll(window, dy, shell, compositor);
                    } else if let (Some(window_id), Ok(x), Ok(y)) = (
                        windows.ipc_id(window),
                        u32::try_from(local.x),
                        u32::try_from(local.y),
                    ) {
                        deliver(
                            server,
                            sink,
                            shell,
                            compositor,
                            windows,
                            picker,
                            &mut apps.service,
                            menu,
                            &WindowEvent::Scrolled {
                                window_id,
                                x,
                                y,
                                dx,
                                dy,
                                modifiers: modifiers_to_abi(modifiers),
                            },
                        );
                    }
                }
                // A step of a pinch that began over a client window: the
                // zoom is the application's. The picker zooms nothing.
                InputResponse::AppPinch {
                    window,
                    local,
                    phase,
                    scale,
                    modifiers,
                } => {
                    if let (Some(window_id), Ok(x), Ok(y)) = (
                        windows.ipc_id(window),
                        u32::try_from(local.x),
                        u32::try_from(local.y),
                    ) {
                        deliver(
                            server,
                            sink,
                            shell,
                            compositor,
                            windows,
                            picker,
                            &mut apps.service,
                            menu,
                            &WindowEvent::Pinch {
                                window_id,
                                x,
                                y,
                                phase,
                                scale,
                                modifiers: modifiers_to_abi(modifiers),
                            },
                        );
                    }
                }
                // A title-bar command control was activated: map it to the
                // window's lifecycle in the one shared place
                // (`window_control_event`) — Close/Minimize/PutToBack/
                // SizeToggle — and deliver the app-ward event it yields
                // (Close→CloseRequested, Minimize→Minimized, SizeToggle→
                // Resized; PutToBack is window-manager-local and yields none)
                // over the existing window path.
                InputResponse::WindowControl { window, control } => {
                    let work_area = shell.work_area(compositor);
                    if let Some(event) =
                        window_control_event(control, window, work_area, shell, compositor, windows)
                    {
                        deliver(
                            server,
                            sink,
                            shell,
                            compositor,
                            windows,
                            picker,
                            &mut apps.service,
                            menu,
                            &event,
                        );
                    }
                    // The trusted picker is the session's own window, so the
                    // shared mapping performs no close for it: what dismissal
                    // *means* is the owner's, and here it means the same as
                    // Escape — the pick is cancelled and the requesting
                    // application is told so.
                    let closing = picker.shown_in(window);
                    if let Some(pick) = closing.filter(|_| control == WindowControlKind::Close) {
                        let step = picker.cancel(pick, shell, compositor);
                        step_pick(
                            step,
                            &Openers { identity, programs },
                            files,
                            server,
                            sink,
                            shell,
                            compositor,
                            windows,
                            picker,
                            apps,
                            menu,
                        );
                    }
                }
                // A secondary press landed on a title-bar control: the window
                // manager changed nothing, so the only outcome is the app-ward
                // event the one shared rule yields (Close→AlternateCloseRequested;
                // every other control, and a window the session itself owns,
                // yields none and the press does nothing at all).
                InputResponse::WindowControlAlternate { window, control } => {
                    if let Some(event) = window_control_alternate_event(control, window, windows) {
                        deliver(
                            server,
                            sink,
                            shell,
                            compositor,
                            windows,
                            picker,
                            &mut apps.service,
                            menu,
                            &event,
                        );
                    }
                }
                // An interactive edge resize-grab moved or settled: tell the
                // owning app its new client content size so it re-lays-out
                // and re-maps its frame region. Every sample is forwarded,
                // not just the last, because a window whose content only
                // catches up when the button comes up is being stretched
                // rather than resized. A size is a value the app converges
                // on, so the hold-back folds a run of them to the newest
                // (`holdback`) and the client's own reader drops the stale
                // ones it has already been sent (`tairix_window`): an app
                // slower than the pointer lags a frame, never a queue.
                InputResponse::Resized { window } | InputResponse::ResizeEnded { window } => {
                    let ended = matches!(response, InputResponse::ResizeEnded { .. });
                    if let Some(event) =
                        resize_drag_event(window, ended, shell, compositor, windows)
                    {
                        deliver(
                            server,
                            sink,
                            shell,
                            compositor,
                            windows,
                            picker,
                            &mut apps.service,
                            menu,
                            &event,
                        );
                    }
                }
                // A client-area pointer motion the window manager consumed no
                // furniture for: forward it to the owning app as a window-local
                // move so its in-content controls track hover and thumb drags.
                // A negative coordinate cannot occur (the router clamps into the
                // client); refuse rather than wrap if it ever did.
                InputResponse::ClientPointerMoved { window, local } => {
                    if let Some(pick) = picker.shown_in(window) {
                        // A press carried off the save panel's icon is the
                        // document itself, carried to where it is saved.
                        if let Some((document, for_window)) =
                            picker.icon_drag(pick, local, compositor.scale())
                        {
                            let carried = (window, pick, for_window);
                            let _ = shell.begin_save_drag(compositor, carried, document);
                        } else {
                            let moved = tairix_wm::InputEvent::PointerMoved { to: local };
                            picker.handle_pointer(pick, local, &moved, shell, compositor);
                        }
                    } else if windows.alerts.owns(window) {
                        // A motion answers nothing: it only lights, drags or
                        // carries a held press.
                        let _ = windows.alerts.pointer(
                            window,
                            local,
                            &tairix_wm::InputEvent::PointerMoved { to: local },
                            at_ns,
                            &mut windows.holds,
                            shell,
                            compositor,
                        );
                    } else if let (Some(id), Ok(x), Ok(y)) = (
                        windows.ipc_id(window),
                        u32::try_from(local.x),
                        u32::try_from(local.y),
                    ) {
                        deliver(
                            server,
                            sink,
                            shell,
                            compositor,
                            windows,
                            picker,
                            &mut apps.service,
                            menu,
                            &WindowEvent::Pointer {
                                window_id: id,
                                x,
                                y,
                                action: PointerAction::Moved,
                                modifiers: pointer_modifiers(shell),
                                at_ns,
                            },
                        );
                    }
                }
                // A primary release that ended a client pointer grab: forward
                // it so an in-content click or drag completes (a tab or combo
                // selection, a released scrollbar thumb).
                InputResponse::ClientPointerReleased { window, local } => {
                    let released = tairix_wm::InputEvent::PointerReleased {
                        button: tairix_wm::PointerButton::Primary,
                    };
                    if let Some(pick) = picker.shown_in(window) {
                        picker.handle_pointer(pick, local, &released, shell, compositor);
                    } else if windows.alerts.owns(window) {
                        let answer = windows.alerts.pointer(
                            window,
                            local,
                            &released,
                            at_ns,
                            &mut windows.holds,
                            shell,
                            compositor,
                        );
                        reconcile_focus(
                            focused,
                            server,
                            sink,
                            shell,
                            compositor,
                            windows,
                            picker,
                            &mut apps.service,
                            menu,
                        );
                        if let Some(answer) = answer {
                            relay_alert_answer(answer, server, sink, shell, windows);
                        }
                    } else if let (Some(id), Ok(x), Ok(y)) = (
                        windows.ipc_id(window),
                        u32::try_from(local.x),
                        u32::try_from(local.y),
                    ) {
                        deliver(
                            server,
                            sink,
                            shell,
                            compositor,
                            windows,
                            picker,
                            &mut apps.service,
                            menu,
                            &WindowEvent::Pointer {
                                window_id: id,
                                x,
                                y,
                                action: PointerAction::Released(
                                    tairix_abi::input::PointerButtonCode::Primary,
                                ),
                                modifiers: pointer_modifiers(shell),
                                at_ns,
                            },
                        );
                    }
                }
                // A drag took a maximized window out of maximization at the
                // size it had: its app is told the state it is in now.
                InputResponse::Unmaximized { window } => {
                    if let Some(event) = settled_size_event(window, compositor, windows) {
                        deliver(
                            server,
                            sink,
                            shell,
                            compositor,
                            windows,
                            picker,
                            &mut apps.service,
                            menu,
                            &event,
                        );
                    }
                }
                // A move is the window manager's own, except a tool window's:
                // its owner is told where the pointer is over the window the
                // palette belongs to, so it can dock it there.
                InputResponse::Moved { window, .. } | InputResponse::MoveEnded { window } => {
                    let ended = matches!(response, InputResponse::MoveEnded { .. });
                    let pointer = shell.router().pointer();
                    if let Some(event) =
                        tool_move_event(window, ended, pointer, compositor, windows)
                    {
                        deliver(
                            server,
                            sink,
                            shell,
                            compositor,
                            windows,
                            picker,
                            &mut apps.service,
                            menu,
                            &event,
                        );
                    }
                }
                // Window-manager-local outcomes the session does not forward
                // app-ward: a scrollbar press.
                InputResponse::Scrolled { .. }
                | InputResponse::FurniturePressed { .. }
                // A pointer motion or key that reached no window belongs to
                // the desktop's icon column, which `route_desktop` has
                // already applied; nothing is forwarded app-ward.
                | InputResponse::DesktopPointerMoved
                | InputResponse::DesktopKey { .. }
                | InputResponse::Ignored => {}
            },
            ShellOutcome::Taskbar(TaskbarResponse::LibraryLaunch { entry }) => {
                // Resolve the chosen entry's bundle through the catalog the
                // popup was handed and spawn its `Run` binary: admitted
                // immediately, loaded on its own task, refusal reported
                // (synchronously here or by the reap), desktop carries on.
                launch_library_entry(
                    shell,
                    compositor,
                    &entry,
                    &mut LaunchCtx {
                        launched,
                        apps: &apps.service,
                        server,
                        sink,
                        windows,
                        identity,
                    },
                );
            }
            ShellOutcome::Taskbar(TaskbarResponse::LibraryLaunchElevated { entry }) => {
                // As the clock's *Set Date & Time…*: the session asks for an
                // account that may run the program, and the console's broker
                // re-authenticates it and starts the program itself.
                let catalog = shell.session().taskbar().library().catalog();
                match catalogued(catalog, &entry) {
                    Ok(chosen) => {
                        let Some(attested) = programs.bundles.identity_of(chosen.bundle().as_str())
                        else {
                            app::report(
                                APP_NAME,
                                format_args!(
                                    "{} was not run as an administrator: no signed manifest \
                                     vouches for what it would run",
                                    chosen.name().as_str()
                                ),
                            );
                            return Routed::Continue;
                        };
                        let request = elevated_launch(chosen, attested);
                        if !elevate.ask(request, elevator, shell, compositor) {
                            io::write_stderr_line(
                                "desktop: could not ask for authorisation; the program was not run",
                            );
                        }
                    }
                    Err(reason) => {
                        app::report_line(&reason);
                    }
                }
            }
            ShellOutcome::Taskbar(TaskbarResponse::LibraryEdit(edit)) => {
                if memory.apply(edit).is_err() {
                    io::write_stderr_line(
                        "desktop: the launcher's record has no room for that change; nothing was kept",
                    );
                }
            }
            ShellOutcome::Taskbar(TaskbarResponse::OpenMenu(request)) => {
                // The bar draws no menu: it hands over a model and an anchor,
                // and the seat's one chain places, draws, grabs and answers it
                // — the same service an application's `OpenMenu` reaches.
                open_bar_menu(
                    request,
                    tairix_desktop_session::seat_modal(lock, (confirm, elevate, resolution)),
                    menu,
                    (shell, compositor),
                    windows,
                    &apps.service,
                );
            }
            ShellOutcome::Taskbar(TaskbarResponse::OpenLibrary) => {
                library_opened(memory, catalogs, (shell, compositor), programs);
            }
            ShellOutcome::Taskbar(TaskbarResponse::AppDefault { app }) => {
                // The application declared that it handles the primary click
                // itself, so the click is relayed to it and the session does
                // nothing else — one click, one actor.
                if let Some(owner) = apps.strip.get(app).map(|group| group.owner) {
                    relay_app_bar(
                        apps,
                        owner,
                        shell,
                        compositor,
                        server,
                        sink,
                        windows,
                        picker,
                        menu,
                        &WindowEvent::AppBarDefault,
                    );
                }
            }
            ShellOutcome::Taskbar(TaskbarResponse::AppRaise { app }) => {
                // No declared default action: raise the application's most
                // recently used window. The bar already refused to report
                // this for an application with none, so there is always one
                // to raise; a window the bridge has since lost changes
                // nothing (fail closed).
                if let Some(window) = mru_window(apps, app, shell) {
                    shell.raise_window(compositor, window);
                }
            }
            ShellOutcome::Taskbar(TaskbarResponse::AppMenuEntered {
                owner,
                item,
                revision,
                text,
            }) => {
                // Held for the application to pull, then announced: the text
                // is wider than an event.
                if let Err(err) = server.record_app_bar_text(owner, item, revision, &text) {
                    app::report(
                        APP_NAME,
                        format_args!("an icon-bar entry could not be held ({err}); not sent"),
                    );
                } else {
                    relay_app_bar(
                        apps,
                        owner,
                        shell,
                        compositor,
                        server,
                        sink,
                        windows,
                        picker,
                        menu,
                        &WindowEvent::AppBarEntered { item, revision },
                    );
                }
            }
            ShellOutcome::Taskbar(TaskbarResponse::AppMenuChosen {
                owner,
                item,
                revision,
            }) => {
                // The row id and the revision of the declaration it was
                // chosen from are the application's own and the session never
                // interprets either: both are relayed straight back to the
                // process that declared the menu, which resolves the id
                // against that declaration rather than a later one.
                relay_app_bar(
                    apps,
                    owner,
                    shell,
                    compositor,
                    server,
                    sink,
                    windows,
                    picker,
                    menu,
                    &WindowEvent::AppBarMenu { item, revision },
                );
            }
            ShellOutcome::Taskbar(TaskbarResponse::OpenSwitchboard { section }) => {
                // The bar already decided which section the gesture asks
                // for (a quick press its overview, a hold its recovery
                // list); the session only relays it to the live monitor.
                // With none live the press is itself the demand for one:
                // bring an instance up and hold the section until its
                // first publish proves it is listening.
                if let Some(revived) = open_tray(
                    pending_open,
                    section,
                    *switchboard,
                    &mut RtSwitchboardMailbox,
                    || {
                        spawn_switchboard(
                            &mut LaunchCtx {
                                launched,
                                apps: &apps.service,
                                server,
                                sink,
                                windows,
                                identity,
                            },
                            shell,
                            compositor,
                        )
                    },
                ) {
                    *switchboard = Some(revived);
                }
            }
            ShellOutcome::Taskbar(TaskbarResponse::LockSession) => lock_screen(
                lock,
                (confirm, elevate, resolution),
                (account, shown_name),
                (shell, compositor, desktop),
                server,
                sink,
            ),
            ShellOutcome::Taskbar(TaskbarResponse::SwitchUser) => {
                // Step aside for another account. The prompt goes down
                // first: an unanswered question must not be left on a screen
                // that is about to belong to somebody else. The switch
                // itself is the loop's, which owns the frame region the
                // session gives back.
                confirm.abandon(shell, compositor);
                report_elevation(elevate.abandon(compositor));
                resolution.abandon(shell, compositor);
                return Routed::SwitchUser;
            }
            ShellOutcome::Taskbar(TaskbarResponse::LogOut) => {
                // The user asked for the session to end: take the prompt down
                // unanswered (so nothing irreversible follows a log-out) and
                // unwind through the one owner-checked release.
                confirm.abandon(shell, compositor);
                report_elevation(elevate.abandon(compositor));
                resolution.abandon(shell, compositor);
                lock.abandon(compositor);
                return Routed::EndSession;
            }
            ShellOutcome::Taskbar(TaskbarResponse::ConfirmSystemPower { action }) => {
                // Never on the strength of the click alone: put the
                // consequence to the user first. A prompt that cannot be
                // shown asks nothing and relays nothing.
                if !confirm.ask(action, tairix_rt::clock_get(), shell, compositor) {
                    io::write_stderr_line(
                        "desktop: could not ask for confirmation; nothing was done",
                    );
                }
            }
            ShellOutcome::Taskbar(TaskbarResponse::AboutSystem) => {
                if !windows.alerts.about(system, shell, compositor) {
                    app::report(
                        APP_NAME,
                        format_args!("no window to state what the system is in"),
                    );
                }
            }
            ShellOutcome::Taskbar(TaskbarResponse::OpenSettings) => {
                let _ = LaunchCtx {
                    launched,
                    apps: &apps.service,
                    server,
                    sink,
                    windows,
                    identity,
                }
                .launch(
                    shell,
                    compositor,
                    SETTINGS_RUN_PATH,
                    SETTINGS_LABEL,
                    &[],
                    None,
                );
            }
            ShellOutcome::Taskbar(TaskbarResponse::OpenNetworkSettings) => {
                // The network's settings are a section of Settings: a running
                // Settings is handed the pane, a fresh one opens on it.
                let _ = LaunchCtx {
                    launched,
                    apps: &apps.service,
                    server,
                    sink,
                    windows,
                    identity,
                }
                .launch(
                    shell,
                    compositor,
                    SETTINGS_RUN_PATH,
                    SETTINGS_LABEL,
                    &[tairix_netconfig::SETTINGS_PANE.as_bytes()],
                    Some(LaunchTarget::Pane(tairix_netconfig::SETTINGS_PANE)),
                );
            }
            ShellOutcome::Taskbar(TaskbarResponse::SetDateTime) => {
                let bundle = tairix_appstore::bundle_of_entry(DATETIME_RUN_PATH);
                // The dialog has no slot to come back through, so asking
                // while it is open raises it rather than starting a second
                // elevated copy.
                let open = bundle.and_then(|bundle| {
                    window_of_bundle(bundle, server, windows, identity, &programs.bundles)
                });
                if open.is_some_and(|wm| shell.raise_window(compositor, wm)) {
                    return Routed::Continue;
                }
                // The session holds no authority to set a clock and never
                // will: it asks for an account that does, and the console's
                // broker re-authenticates it and starts the application
                // itself. A prompt that cannot be shown asks nothing and
                // sets nothing.
                let request = AuthoriseRequest {
                    program: alloc::string::String::from(DATETIME_RUN_PATH),
                    elevation: Elevation::Launch,
                    requester: alloc::string::String::from(DATETIME_TITLE),
                    bundle: bundle.map(alloc::string::String::from),
                    purpose: alloc::string::String::from(SET_TIME_PURPOSE),
                    asker: None,
                };
                if !elevate.ask(request, elevator, shell, compositor) {
                    io::write_stderr_line(
                        "desktop: could not ask for authorisation; the clock was not changed",
                    );
                }
            }
            // An event no router acted on; outcomes the shell has already
            // fully applied with its own state (the click-to-activate/minimise
            // rule, clearing a dismissed notification from the model, the
            // popup's own open/close, opening the hover picker out of the
            // thumbnails it prepared); and the desktop shortcut, which
            // `route_desktop` — the owner of that folder, with its one
            // creation path — has already taken. Nothing here needs a
            // capability this side of the routing holds, so the session adds
            // nothing. Listed rather than caught by a wildcard so a new
            // outcome fails the build instead of being dropped in silence.
            ShellOutcome::Ignored
            | ShellOutcome::Taskbar(
                TaskbarResponse::Ignored
                | TaskbarResponse::LibraryDismissed
                | TaskbarResponse::WindowChosen { .. }
                | TaskbarResponse::DismissNotification { .. }
                | TaskbarResponse::ShowWindowPicker { .. }
                | TaskbarResponse::CreateDesktopShortcut { .. }
                | TaskbarResponse::Sound(_)
                | TaskbarResponse::StopScreenRecording,
            ) => {}
        }
        Routed::Continue
    }

    /// The records this session keeps in its private scope beside the sound
    /// choices: the launcher's, and where the user put their desktop icons.
    struct PrivateRecords {
        launcher: LauncherMemory,
        arrangement: ArrangementMemory,
    }

    impl PrivateRecords {
        /// Neither record read yet, each read due at once.
        fn new() -> Self {
            Self {
                launcher: LauncherMemory::new(),
                arrangement: ArrangementMemory::new(),
            }
        }

        /// `park_ns` shortened to when either record's next read is due.
        fn park_deadline_ns(&self, now_ns: u64, park_ns: u64) -> u64 {
            let park = self.launcher.park_deadline_ns(now_ns, park_ns);
            self.arrangement.park_deadline_ns(now_ns, park)
        }
    }

    /// The private-store jobs the session asks for itself, each held one at a
    /// time: the sound choices' load and push, and each record's read and
    /// write.
    const OWN_PRIVATE_JOBS: usize = 6;

    /// The most private-store jobs held at once: the session's own, and the
    /// pinboard calls it holds. A containment bound, not a capacity: a call past
    /// it is answered busy, and the session's own always fit.
    const PRIVATE_JOBS: usize = OWN_PRIVATE_JOBS + MAX_HELD_CALLS;

    /// One round trip in this session's private scope: each is a store, a
    /// disk or another service away.
    enum PrivateJob {
        /// Read the launcher's record.
        LauncherLoad,
        /// Write the launcher record's changes, the ask `number`.
        LauncherStore {
            number: u64,
            changes: Vec<(alloc::string::String, Option<alloc::string::String>)>,
        },
        /// Read where the user put their desktop icons.
        ArrangementLoad,
        /// Write the changes to where the icons stand, the ask `number`.
        ArrangementStore {
            number: u64,
            changes: Vec<(alloc::string::String, Option<alloc::string::String>)>,
        },
        /// Read the stored choices.
        Load,
        /// Store `choices`, the ask `number`, answering `ticket` once stored.
        Store {
            number: u64,
            choices: UserChoices,
            ticket: Option<u64>,
        },
        /// Delegate the files `choices` names to the cue authority's instance
        /// `authority`, for the push the loop then posts.
        Push {
            authority: ProcId,
            choices: UserChoices,
        },
        /// Answer the pane's read `ticket` from `ask`, over the machine's
        /// document as it is now.
        Read { ask: ReadAsk, ticket: u64 },
    }

    /// What a private-store round trip came to.
    enum PrivateAnswer {
        /// The launcher's record, read.
        LauncherLoaded(LoadedPersonal),
        /// What the launcher record's write `number` left the store holding.
        LauncherStored {
            number: u64,
            result: Result<Vec<(alloc::string::String, Option<alloc::string::String>)>, Errno>,
        },
        /// Where the user put their desktop icons, read.
        ArrangementLoaded(LoadedArrangement),
        /// What the arrangement's write `number` left the store holding.
        ArrangementStored {
            number: u64,
            result: Result<Vec<(alloc::string::String, Option<alloc::string::String>)>, Errno>,
        },
        Loaded(LoadedChoices),
        Stored {
            number: u64,
            result: Result<UserChoices, Errno>,
            ticket: Option<u64>,
        },
        /// A push's files, delegated: the document it carries, each event's
        /// handle, and which could not be handed over.
        Handed {
            document: alloc::string::String,
            files: [u64; SoundEvent::ALL.len()],
            unavailable: [bool; SoundEvent::ALL.len()],
        },
        /// A pane's read, answered on the worker.
        Answered,
    }

    type PrivateWorker = Worker<
        (),
        PrivateJob,
        PrivateAnswer,
        tairix_util::defer::JobQueue<PrivateJob, PrivateAnswer>,
    >;

    fn serve_private((): &mut (), job: &mut PrivateJob) -> PrivateAnswer {
        match job {
            PrivateJob::LauncherLoad => PrivateAnswer::LauncherLoaded(load_personal(&mut RtHost)),
            PrivateJob::LauncherStore { number, changes } => PrivateAnswer::LauncherStored {
                number: *number,
                result: store_personal(&mut RtHost, changes),
            },
            PrivateJob::ArrangementLoad => {
                PrivateAnswer::ArrangementLoaded(load_arrangement(&mut RtHost))
            }
            PrivateJob::ArrangementStore { number, changes } => PrivateAnswer::ArrangementStored {
                number: *number,
                result: store_personal(&mut RtHost, changes),
            },
            PrivateJob::Load => PrivateAnswer::Loaded(load_choices(&mut RtHost)),
            PrivateJob::Store {
                number,
                choices,
                ticket,
            } => PrivateAnswer::Stored {
                number: *number,
                result: store_choices(&mut RtHost, choices),
                ticket: *ticket,
            },
            PrivateJob::Push { authority, choices } => {
                let (files, unavailable) = hand_over_files(*authority, choices);
                PrivateAnswer::Handed {
                    document: choices.pushed(),
                    files,
                    unavailable,
                }
            }
            PrivateJob::Read { ask, ticket } => {
                let machine = machine_sound_settings();
                let reading = ask.reading(&machine);
                let mut frame = [0u8; PINBOARD_MAX_REPLY];
                let len = encode_sound_reading(Ok(&reading.as_wire()), &mut frame)
                    .or_else(|err| encode_sound_reading(Err(err), &mut frame));
                if let Ok(len) = len {
                    reply_call(PINBOARD_ENDPOINT, *ticket, &frame[..len]);
                }
                PrivateAnswer::Answered
            }
        }
    }

    /// The machine's sound document as it is now, read tolerantly: the shipped
    /// settings where there is none.
    fn machine_sound_settings() -> SoundSettings {
        read_file(
            tairix_soundtheme::MACHINE_SETTINGS,
            tairix_appconf::MAX_DOCUMENT_LEN,
        )
        .ok()
        .and_then(|bytes| alloc::string::String::from_utf8(bytes).ok())
        .and_then(|text| tairix_appconf::Document::parse(&text).ok())
        .map(|document| <SoundSettings as tairix_appconf::Registry>::load(&document).0)
        .unwrap_or_default()
    }

    /// Delegate each file `choices` names to the cue authority `authority`,
    /// read-only: each event's handle, and which could not be handed over.
    fn hand_over_files(
        authority: ProcId,
        choices: &UserChoices,
    ) -> ([u64; SoundEvent::ALL.len()], [bool; SoundEvent::ALL.len()]) {
        let mut unavailable = [false; SoundEvent::ALL.len()];
        let mut files = [0u64; SoundEvent::ALL.len()];
        for (at, event) in SoundEvent::ALL.into_iter().enumerate() {
            let Some(path) = choices.file(event) else {
                continue;
            };
            match hand_over(path, authority) {
                Ok(handle) => files[at] = handle,
                Err(err) => {
                    unavailable[at] = true;
                    app::report(
                        APP_NAME,
                        format_args!(
                            "the file chosen for the {} sound could not be handed over ({err}); \
                             its theme's plays",
                            event.name()
                        ),
                    );
                }
            }
        }
        (files, unavailable)
    }

    /// Post the push whose files are delegated, its answer collected on the
    /// cue authority's wake.
    fn post_push(effects: &mut Effects, cue_posts: &mut tairix_soundtheme::PostedCues, set: u64) {
        let Some((document, files)) = effects.take_post() else {
            return;
        };
        if let Err(err) = cue_posts.push(
            &mut tairix_soundtheme::cue::RtCuePost::new(set, CUE_TOKEN),
            &document,
            files,
            tairix_soundtheme::POLICY_ANSWER_DEADLINE_NS,
        ) {
            refused_push(effects.pushed(tairix_rt::clock_get(), Err(err)));
        }
    }

    /// State why the cue authority did not take the user's sound choices, if
    /// it did not.
    fn refused_push(refusal: Option<Errno>) {
        if let Some(err) = refusal {
            app::report(
                APP_NAME,
                format_args!("the cue authority did not take the sound choices ({err})"),
            );
        }
    }

    /// Delegate the file at `path` to `to`, read-only, if it is a regular file
    /// a sound may be read from. The delegation names the path the user chose
    /// and re-opens it as they could, so the descriptor is closed at once.
    fn hand_over(path: &str, to: ProcId) -> Result<u64, Errno> {
        let file = tairix_rt::File::open(path.as_bytes(), tairix_abi::fs::OpenFlags::READ)
            .map_err(Errno::from_syscall)?;
        let stat = file.stat().map_err(Errno::from_syscall)?;
        if stat.kind != tairix_abi::fs::FileKind::Regular {
            return Err(Errno::OutOfRange);
        }
        if usize::try_from(stat.size).map_or(true, |len| len > tairix_soundtheme::MAX_SOUND_BYTES) {
            return Err(Errno::LengthOutOfRange);
        }
        Errno::syscall_result(tairix_rt::fd_grant(file.fd(), 0, to, 0))
    }

    /// Hand `work` to the sound-choices worker, adopting at once what ran here
    /// for want of one; work it has no room for is refused with its reason.
    fn submit_effects(
        effects: &mut Effects,
        memory: &mut PrivateRecords,
        worker: &PrivateWorker,
        work: EffectsWork,
        now: u64,
    ) {
        let job = match work {
            EffectsWork::Store {
                number,
                choices,
                ticket,
            } => {
                if ticket.is_some() && !effects.admit_call() {
                    effects.stored(number, Err(Errno::Busy));
                    if let Some(ticket) = ticket {
                        reply_pinboard(ticket, Err(Errno::Busy));
                    }
                    return;
                }
                PrivateJob::Store {
                    number,
                    choices,
                    ticket,
                }
            }
            EffectsWork::Push { authority, choices } => PrivateJob::Push { authority, choices },
        };
        match worker.submit(job) {
            Ok(true) => pump_private(effects, memory, worker, now),
            Ok(false)
            | Err(
                PrivateJob::Load
                | PrivateJob::Read { .. }
                | PrivateJob::LauncherLoad
                | PrivateJob::LauncherStore { .. }
                | PrivateJob::ArrangementLoad
                | PrivateJob::ArrangementStore { .. },
            ) => {}
            Err(PrivateJob::Store { number, ticket, .. }) => {
                effects.stored(number, Err(Errno::Busy));
                if let Some(ticket) = ticket {
                    effects.release_call();
                    reply_pinboard(ticket, Err(Errno::Busy));
                }
            }
            Err(PrivateJob::Push { .. }) => {
                effects.pushed(now, Err(Errno::Busy));
            }
        }
    }

    /// Carry a pick the session made for itself into the user's sound
    /// choices, answering the call that asked for it.
    fn conclude_own_pick(
        own: OwnPick,
        effects: &mut Effects,
        memory: &mut PrivateRecords,
        worker: &PrivateWorker,
    ) {
        let Requester::Sound(_) = own.requester else {
            return;
        };
        let Some(path) = own.path else {
            if let Some(ticket) = effects.cancelled() {
                reply_pinboard(ticket, Ok(()));
            }
            return;
        };
        if let Ok(path) = tairix_appconf::SettingPath::new(&path) {
            if let Some(work) = effects.chosen(&path) {
                submit_effects(effects, memory, worker, work, tairix_rt::clock_get());
            }
        } else {
            app::report(
                APP_NAME,
                "the chosen file's path is not one a setting can name; nothing was chosen",
            );
            if let Some(ticket) = effects.cancelled() {
                reply_pinboard(ticket, Err(Errno::LengthOutOfRange));
            }
        }
    }

    /// Adopt every private-store answer that has come back — the sound
    /// choices', the launcher record's and the arrangement's — then ask for
    /// each read, push and write owed.
    #[allow(clippy::too_many_lines)] // One arm per record's answer; split, the order they land in would be hidden.
    fn pump_private(
        effects: &mut Effects,
        memory: &mut PrivateRecords,
        worker: &PrivateWorker,
        now: u64,
    ) {
        worker.collect_landed(|answer| match answer {
            PrivateAnswer::LauncherLoaded(loaded) => {
                for warning in &loaded.warnings {
                    app::report_line(warning);
                }
                memory.launcher.loaded(now, loaded.personal);
            }
            PrivateAnswer::LauncherStored { number, result } => {
                if let Err(err) = &result {
                    app::report(
                        APP_NAME,
                        format_args!("the launcher's record could not be kept ({err})"),
                    );
                }
                for key in memory.launcher.stored(number, result) {
                    app::report(
                        APP_NAME,
                        format_args!("the launcher record `{key}` reads back as none kept"),
                    );
                }
            }
            PrivateAnswer::ArrangementLoaded(loaded) => {
                for warning in &loaded.warnings {
                    app::report_line(warning);
                }
                memory.arrangement.loaded(now, loaded.arrangement);
            }
            PrivateAnswer::ArrangementStored { number, result } => {
                if let Err(err) = &result {
                    app::report(
                        APP_NAME,
                        format_args!("where the icons were put could not be kept ({err})"),
                    );
                }
                for key in memory.arrangement.stored(number, result) {
                    app::report(
                        APP_NAME,
                        format_args!("the icon place `{key}` reads back as none kept"),
                    );
                }
            }
            PrivateAnswer::Loaded(loaded) => {
                for warning in &loaded.warnings {
                    app::report_line(warning);
                }
                effects.loaded(now, loaded.choices);
            }
            PrivateAnswer::Stored {
                number,
                result,
                ticket,
            } => {
                if let Err(err) = &result {
                    app::report(
                        APP_NAME,
                        format_args!("the sound choices could not be stored ({err})"),
                    );
                }
                let outcome = result.as_ref().map(|_| ()).map_err(|err| *err);
                effects.stored(number, result);
                if let Some(ticket) = ticket {
                    effects.release_call();
                    reply_pinboard(ticket, outcome);
                }
            }
            PrivateAnswer::Handed {
                document,
                files,
                unavailable,
            } => effects.handed_over(document, files, unavailable),
            PrivateAnswer::Answered => effects.release_call(),
        });
        if effects.next_load(now) {
            match worker.submit(PrivateJob::Load) {
                Ok(true) => pump_private(effects, memory, worker, now),
                Ok(false) => {}
                Err(_) => effects.loaded(now, None),
            }
        }
        if let Some(work) = effects.next_push(now) {
            submit_effects(effects, memory, worker, work, now);
        }
        if memory.launcher.next_load(now) {
            match worker.submit(PrivateJob::LauncherLoad) {
                Ok(true) => pump_private(effects, memory, worker, now),
                Ok(false) => {}
                Err(_) => memory.launcher.loaded(now, None),
            }
        }
        if let Some(ask) = memory.launcher.next_store() {
            let number = ask.number;
            match worker.submit(PrivateJob::LauncherStore {
                number,
                changes: ask.changes,
            }) {
                Ok(true) => pump_private(effects, memory, worker, now),
                Ok(false) => {}
                // Nothing was asked: the edits ride with the next write.
                Err(_) => memory.launcher.unasked(number),
            }
        }
        if memory.arrangement.next_load(now) {
            match worker.submit(PrivateJob::ArrangementLoad) {
                Ok(true) => pump_private(effects, memory, worker, now),
                Ok(false) => {}
                Err(_) => memory.arrangement.loaded(now, None),
            }
        }
        if let Some(ask) = memory.arrangement.next_store() {
            let number = ask.number;
            match worker.submit(PrivateJob::ArrangementStore {
                number,
                changes: ask.changes,
            }) {
                Ok(true) => pump_private(effects, memory, worker, now),
                Ok(false) => {}
                Err(_) => memory.arrangement.unasked(number),
            }
        }
    }

    /// The cue authority's instance, as its notice says now.
    fn cue_authority() -> Option<ProcId> {
        let mut buf = [0u8; tairix_abi::NOTICE_PAYLOAD_MAX];
        let read =
            usize::try_from(tairix_rt::notice_read(NoticeTopic::CueAuthority, &mut buf)).ok()?;
        match Notice::decode(NoticeTopic::CueAuthority, buf.get(..read)?) {
            Ok(Notice::CueAuthority { instance }) => Some(instance),
            _ => None,
        }
    }

    /// Answer the pane's request `ticket` for the sound themes from `from`.
    fn answer_themes(themes: &[alloc::string::String], from: u16, ticket: u64) {
        let names: Vec<&str> = themes
            .iter()
            .skip(usize::from(from))
            .take(usize::from(SOUND_THEMES_PAGE))
            .map(alloc::string::String::as_str)
            .collect();
        let mut frame = [0u8; PINBOARD_MAX_REPLY];
        if let Ok(len) = encode_sound_themes(&names, &mut frame) {
            reply_call(PINBOARD_ENDPOINT, ticket, &frame[..len]);
        }
    }

    /// The session's round trips to the audio service.
    type SoundWorker = Worker<RtAudio, SoundJob, SoundAnswer>;

    /// Apply a job's controls in order, then list the sinks and the sources.
    fn serve_sound(audio: &mut RtAudio, job: &mut SoundJob) -> SoundAnswer {
        let mut refused = None;
        for (device_id, control) in job.controls.drain(..) {
            if let Err(err) = set_control(audio, device_id, control) {
                refused.get_or_insert(err);
            }
        }
        let listed = devices(audio, StreamDirection::Playback).and_then(|mut sinks| {
            sinks.append(&mut devices(audio, StreamDirection::Capture)?);
            Ok(sinks)
        });
        SoundAnswer {
            refused,
            devices: listed,
        }
    }

    /// Adopt every round trip already back and start the next one owed: the
    /// bar shows what came back, a change the user settled is heard at the
    /// output's new level, posted through `cue_posts` on the wait-set `set`,
    /// and what the user's own room shows of their controls is published with
    /// their settings.
    #[allow(clippy::too_many_arguments)] // The desktop's settings state, threaded explicitly.
    fn pump_sound<S: DirectorySource>(
        sound: &mut SoundSession,
        worker: &SoundWorker,
        cue_posts: &mut tairix_soundtheme::PostedCues,
        set: u64,
        publisher: &Publisher,
        pinboard: &mut PinboardPanel,
        wallpapers: &Wallpapers,
        desktop: &mut Desktop<S>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        loop {
            while let Some(answer) = worker.collect() {
                let landed = sound.landed(answer, &desktop.settings().sound);
                shell.set_sound(compositor, landed.shown);
                if landed.confirmed {
                    // Posted, never awaited: refused or silent, the answer is
                    // the authority's to record.
                    let _ = cue_posts.post(
                        &mut tairix_soundtheme::cue::RtCuePost::new(set, CUE_TOKEN),
                        SoundEvent::VolumeFeedback,
                        tairix_soundtheme::Answer::Started,
                        tairix_soundtheme::CUE_ANSWER_DEADLINE_NS,
                    );
                }
                if let Some(err) = landed.refused {
                    app::report(
                        APP_NAME,
                        format_args!("a sound control was refused ({err})"),
                    );
                }
                if let Some(remembered) = landed.remember {
                    let mut settings = publisher.base(desktop.settings());
                    settings.sound = remembered;
                    let _ = request_pinboard_settings(
                        settings,
                        publisher,
                        None,
                        pinboard,
                        wallpapers,
                        desktop,
                        shell,
                        compositor,
                        tairix_rt::clock_get(),
                    );
                }
            }
            let Some(job) = sound.next_job() else {
                return;
            };
            // On a thread the answer wakes the loop; inline it is already
            // back, and is adopted on the next turn round.
            if !worker.submit(job) {
                return;
            }
        }
    }

    /// What the set-date-and-time request names as asking: the program that
    /// runs once it is allowed.
    const DATETIME_TITLE: &str = "Date & Time";

    /// The most accounts a listing gathers, a page at a time: a containment
    /// bound on what one answer may make the session hold, far past any
    /// machine's count of accounts that may allow one program.
    const MOST_AUTHORISERS: usize = 256;

    /// The thread the authorisation prompt's questions go to the broker on,
    /// so the key it derives and the program it runs never stall the
    /// compositor.
    type ElevateWorker = Worker<(), ElevateAsk, AuthoriseAnswer>;

    /// The production elevation seam: ask this console's broker, on its
    /// worker, who may allow a program and whether a password allows it.
    ///
    /// The desktop never authenticates anybody and never runs the program
    /// itself; it carries the question and reads the answer.
    struct RtElevator {
        worker: alloc::sync::Arc<ElevateWorker>,
        tickets: Tickets,
        /// The signed-in account's login name.
        own: alloc::string::String,
        /// The answer to a question no memory could hold, owed before any
        /// the worker brings back.
        unasked: Option<AuthoriseAnswer>,
    }

    impl RtElevator {
        /// The oldest answer that has landed, taken once.
        fn landed(&mut self) -> Option<AuthoriseAnswer> {
            self.unasked.take().or_else(|| self.worker.collect())
        }
    }

    impl Elevator for RtElevator {
        fn authorisers(&mut self, program: &str) -> Ticket {
            let ticket = self.tickets.issue();
            self.worker.submit(ElevateAsk::Authorisers {
                ticket,
                program: alloc::string::String::from(program),
                own: self.own.clone(),
            });
            ticket
        }

        fn offer(&mut self, username: &str, password: &str, request: &AuthoriseRequest) -> Ticket {
            let ticket = self.tickets.issue();
            match ElevateAsk::offer(ticket, username, password, request) {
                Some(ask) => {
                    self.worker.submit(ask);
                }
                None => {
                    self.unasked = Some(AuthoriseAnswer::Offer(
                        ticket,
                        Err(AuthoriseRefusal::NotDone(Errno::OutOfMemory)),
                    ));
                }
            }
            ticket
        }
    }

    /// The font worker: one thread that may wait on the font service, with
    /// no state of its own, answering whether a fetch landed anything.
    type FontWorker = Worker<(), (), bool>;

    /// What says whether the desktop's text is drawn with its real glyphs.
    #[derive(Copy, Clone)]
    struct TextDrawn<'a> {
        fonts: &'a FontWorker,
        /// What deferred fetches had brought when the text was last laid out.
        generation: u64,
    }

    impl TextDrawn<'_> {
        /// Whether all of it is: nothing drawn provisionally waits to be
        /// fetched, no fetch is out, and what the last one brought has been
        /// laid out again.
        fn settled(self) -> bool {
            !tairix_font::has_deferred()
                && !self.fonts.outstanding()
                && tairix_font::text_generation() == self.generation
        }
    }

    /// The font worker's job: fetch what the desktop drew provisionally.
    fn fetch_fonts(_: &mut (), _: &mut ()) -> bool {
        tairix_font::fetch_deferred()
    }

    /// Ask the font worker for what the desktop drew provisionally, if it
    /// drew anything: a request already waiting absorbs this one.
    fn fetch_drawn_text(font_worker: &FontWorker) {
        if tairix_font::has_deferred() {
            let _ = font_worker.submit(());
        }
    }

    /// Lay out and draw again whatever was drawn from a provisional font
    /// answer, now that what it missed has landed. The prompts and alerts are
    /// laid out again by the caller, beside a change of look.
    fn refit_text<S: DirectorySource>(
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        desktop: &Desktop<S>,
        (menu, windows): (&mut MenuChain, &SessionWindows),
        lock: &mut ScreenLock,
    ) {
        shell.session_mut().taskbar_mut().redraw_text();
        compositor.refresh_titles();
        shell.present(compositor);
        shell.present_desktop_text(compositor, desktop);
        shell.relay_drag(compositor);
        if menu.refit(&chain_geometry(shell.session(), compositor)) {
            present_menu_chain(menu, shell, compositor, windows);
        }
        shell.present_tooltip(compositor);
        lock.repaint(shell, compositor);
    }

    /// The broker's answer to one question at the authorisation prompt, any
    /// password erased before returning.
    fn ask_broker_for(_: &mut (), ask: &mut ElevateAsk) -> AuthoriseAnswer {
        let ticket = ask.ticket();
        let answer = match ask {
            ElevateAsk::Authorisers { program, own, .. } => {
                AuthoriseAnswer::Listing(ticket, authorisers_of(program, own))
            }
            ElevateAsk::Offer { .. } => AuthoriseAnswer::Offer(ticket, perform(ask)),
        };
        ask.erase();
        answer
    }

    /// Every account the broker says may allow `program`, a page at a time
    /// up to [`MOST_AUTHORISERS`]; `None` when it could not say.
    ///
    /// The broker lists the caller's own account first when it may, so the
    /// first account of the first page is taken as `own`'s only when it is
    /// `own`: a page that left it out cannot have someone else shown as the
    /// signed-in account.
    fn authorisers_of(program: &str, own: &str) -> Option<Authorisers> {
        let mut listing = Authorisers::default();
        let mut offset = 0u32;
        loop {
            let mut reply = [0u8; ELEVATE_MAX_REPLY];
            let ElevateReply::Authorisers { own: may, page } =
                tairix_rt::elevate(&ElevateRequest::Authorisers { program, offset }, &mut reply)
                    .ok()?
            else {
                return None;
            };
            let page = decode_account_page(page).ok()?;
            let mut accounts = page
                .accounts()
                .iter()
                .map(|account| AccountTile::new(account.display_name(), account.login_name()))
                .peekable();
            if offset == 0 && may && accounts.peek().is_some_and(|tile| tile.login_name() == own) {
                listing.own = accounts.next();
            }
            listing.others.extend(accounts);
            let read = u32::try_from(page.accounts().len()).ok()?;
            offset = offset.checked_add(read)?;
            if page.is_last() || read == 0 || listing.others.len() >= MOST_AUTHORISERS {
                listing.others.truncate(MOST_AUTHORISERS);
                return Some(listing);
            }
        }
    }

    /// Have the broker perform an offered request, answering what it did or
    /// why it did nothing. A password that is not text was never one the
    /// broker could check.
    fn perform(ask: &ElevateAsk) -> Result<AuthoriseDone, AuthoriseRefusal> {
        let ElevateAsk::Offer {
            username, request, ..
        } = ask
        else {
            return Err(AuthoriseRefusal::Unreachable);
        };
        let password = Secret::new(ask.password().ok_or(AuthoriseRefusal::Refused)?);
        let program = request.program.as_str();
        let words: Vec<&str> = match &request.elevation {
            Elevation::Launch | Elevation::Wired => Vec::new(),
            Elevation::Run(words) | Elevation::Capture(words) => {
                words.iter().map(alloc::string::String::as_str).collect()
            }
        };
        let argv = ElevateArgv::new(&words).map_err(AuthoriseRefusal::NotDone)?;
        let wire = match request.elevation {
            Elevation::Launch => ElevateRequest::Launch {
                username,
                password,
                program,
            },
            Elevation::Wired => ElevateRequest::LaunchWired {
                username,
                password,
                program,
            },
            Elevation::Run(_) => ElevateRequest::Run {
                username,
                password,
                program,
                argv,
            },
            Elevation::Capture(_) => ElevateRequest::Capture {
                username,
                password,
                program,
                argv,
            },
        };
        let mut reply = [0u8; ELEVATE_MAX_REPLY];
        match tairix_rt::elevate(&wire, &mut reply) {
            Ok(ElevateReply::Launched { pid }) => Ok(AuthoriseDone::Launched { pid }),
            Ok(ElevateReply::Wired(launch)) if request.elevation == Elevation::Wired => {
                Ok(AuthoriseDone::Wired(launch))
            }
            // A recorder started for a question this session did not ask is
            // let go rather than left running where nothing can reach it.
            Ok(ElevateReply::Wired(launch)) => {
                let_go_recorder(launch);
                Err(AuthoriseRefusal::Unreachable)
            }
            Ok(ElevateReply::Completed { exit_code }) => Ok(AuthoriseDone::Ran { exit_code }),
            Ok(ElevateReply::Captured { exit_code, output }) => Ok(AuthoriseDone::Captured {
                exit_code,
                output: output.to_vec(),
            }),
            Ok(ElevateReply::Overran { exit_code }) => Ok(AuthoriseDone::Overran { exit_code }),
            Ok(ElevateReply::Refused(Errno::PermissionDenied)) => Err(AuthoriseRefusal::Refused),
            Ok(ElevateReply::Refused(err)) => Err(AuthoriseRefusal::NotDone(err)),
            Ok(ElevateReply::Cooling { retry_after }) => {
                Err(AuthoriseRefusal::Cooling(retry_after))
            }
            // A reply to a question this session did not ask: it was not
            // spoken to in this protocol, and nothing is taken from it.
            Ok(ElevateReply::Verified | ElevateReply::Authorisers { .. }) | Err(_) => {
                Err(AuthoriseRefusal::Unreachable)
            }
        }
    }

    /// Adopt every answer the broker has given the authorisation prompt, and
    /// keep the one a recording asked for itself.
    ///
    /// An administrator's recorder the prompt no longer waits on — its
    /// question taken down while the broker started it — is let go, so it
    /// ends rather than running on where nothing can reach it.
    fn adopt_elevations(
        elevator: &mut RtElevator,
        (elevate, recorder): (&mut AuthorisePrompt, &mut Recorder),
        shell: &DesktopShell,
        compositor: &mut Compositor,
    ) {
        while let Some(answer) = elevator.landed() {
            let Some(answer) = recorder.hear(answer) else {
                continue;
            };
            let stray = answer.wired();
            let now_ns = tairix_rt::clock_get();
            let outcome = elevate.answer(answer, now_ns, shell, compositor);
            if let (Some(launch), PromptOutcome::Pending) = (stray, &outcome) {
                let_go_recorder(launch);
            }
            report_elevation(outcome);
        }
    }

    /// State a concluded elevation on `stderr`.
    ///
    /// The started program is deliberately **not** entered in the launch
    /// table. That table maps a child the session started to the bundle it
    /// came from, and an entry leaves it only when the session *reaps* that
    /// child — but an elevated program is login's child, so the reap never
    /// comes and the entry would outlive the program for the life of the
    /// session, claiming a bundle still runs. Its window therefore resolves
    /// its identity exactly as any other window the session did not launch
    /// does (a program started from a shell, say): one uniform behaviour,
    /// rather than a special case that leaks.
    ///
    /// A refusal is already stated in the prompt, which stays up for another
    /// attempt, so nothing is reported for one here. A request that was not
    /// allowed is reported, since one that silently came to nothing leaves no
    /// trace of why.
    fn report_elevation(outcome: PromptOutcome) {
        match outcome {
            PromptOutcome::Done { .. }
            | PromptOutcome::Pending
            | PromptOutcome::Cancelled { request: None } => {}
            PromptOutcome::Cancelled {
                request: Some(request),
            } => {
                app::report(
                    APP_NAME,
                    format_args!("{} was not allowed; nothing was changed", request.program),
                );
            }
        }
    }

    /// Relay a confirmed power transition over the production mailbox and
    /// state loudly why nothing happened when it could not be relayed.
    fn report_power_relay(answer: Answer, switchboard: Option<u64>) {
        if let Some(reason) = relay_power(answer, switchboard, &mut RtSwitchboardMailbox) {
            app::report(APP_NAME, format_args!("{reason}"));
        }
    }

    /// Relay one icon-bar event to the application whose slot it landed on.
    ///
    /// The destination is the declaration the window engine recorded for the
    /// slot's attested owner, never anything the event carries, so a bar
    /// event can only ever reach the process that asked to be on the bar. An
    /// application with no declaration — a slot the session derived from its
    /// windows alone — has nothing to relay to, and a refused delivery tears
    /// its windows down exactly as any other refused send does.
    #[allow(clippy::too_many_arguments)] // The delivery path's whole mutable state, threaded explicitly.
    fn relay_app_bar<S: DirectorySource, F: PickSources<S>>(
        apps: &mut AppBarPanel,
        owner: ProcId,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        server: &mut WindowServer<RtShmMapper>,
        sink: &mut RtEventSink,
        windows: &mut SessionWindows,
        picker: &mut SessionPicker<S, F>,
        menu: &mut MenuChain,
        event: &WindowEvent,
    ) {
        let mut owner_hex = [0u8; tairix_abi::PROC_ID_HEX_LEN];
        log_info(
            tairix_desktop_session::APP_BAR_RELAYED,
            "desktop: icon-bar action relayed to its application",
            &[
                LogField {
                    key: "owner",
                    value: LogFieldValue::Str(owner.write_hex(&mut owner_hex)),
                },
                LogField {
                    key: "action",
                    value: LogFieldValue::Str(match event {
                        WindowEvent::AppBarDefault => "default",
                        WindowEvent::AppBarMenu { .. } => "menu",
                        WindowEvent::AppBarEntered { .. } => "entered",
                        _ => "other",
                    }),
                },
            ],
        );
        let activation = Activation::Granted {
            focus: windows.focused(shell),
        };
        if let Err(Errno::NotFound) = server.deliver_app_event(sink, owner, event, activation) {
            // The declaration is gone with the process: its windows go too,
            // exactly as a refused window-scoped send tears them down.
            let mut bridge = ShellWindowHost {
                shell,
                compositor,
                windows,
                picker,
                apps: &mut apps.service,
                menu,
                // This bridge tears windows down and never serves an
                // `OpenMenu`, so it cannot vouch for the seat and says so.
                seat_held: true,
                screensaver: None,
                relay: &mut RtDocumentRelay,
                wallpapers: &mut NoGallery,
                cursor_sets: &[],
                clipboard: &mut tairix_desktop_session::clipboard::NoClipboard,
                authorise: None,
            };
            server.client_exited(&mut bridge, owner);
        }
    }

    /// The window the icon-bar slot at `app` raises: its application's most
    /// recently used one.
    ///
    /// "Most recently used" is the window list's own answer — the focused
    /// window when this application owns it, else the one it handed focus to
    /// last, else the newest it opened — so the bar and the Switchboard
    /// capsule agree on what "the last window you were in" means.
    fn mru_window(
        apps: &AppBarPanel,
        app: usize,
        shell: &DesktopShell,
    ) -> Option<tairix_wm::WindowId> {
        let group = apps.strip.get(app)?;
        let tasks = shell.session().taskbar().tasks();
        let owned = |task: Option<TaskId>| task.filter(|task| group.windows.contains(task));
        let task = owned(tasks.focused())
            .or_else(|| owned(tasks.previous()))
            .or_else(|| group.windows.last().copied())?;
        shell.tasks().window_for(task)
    }

    /// Adopt a program-catalogue snapshot: state whatever could not be used,
    /// hand the catalog to the taskbar's popup, warm its icon artwork, and
    /// replace the file-type associations.
    ///
    /// The catalog and the associations arrive together because the
    /// associations are read from the bundles that very catalog names, so a
    /// click can never resolve a bundle against a catalog it was not read
    /// from.
    fn adopt_programs(
        loaded: LoadedPrograms,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        programs: &mut Programs,
    ) {
        for warning in &loaded.warnings {
            app::report_line(warning);
        }
        // Read in the same scan as the catalog, so a program's stated purpose
        // can never be another bundle's.
        shell.set_library_facts(loaded.bundles.library_facts(&loaded.catalog));
        shell.set_library(compositor, loaded.catalog);
        shell.warm_icon_artwork(compositor);
        programs.associations = loaded.associations;
        // Latched only on a real change, so the strip is re-resolved when an
        // identity actually became knowable rather than once per rescan.
        programs.adopted |= programs.bundles != loaded.bundles;
        programs.bundles = loaded.bundles;
        shell.set_settings_installed(
            tairix_appstore::bundle_of_entry(SETTINGS_RUN_PATH)
                .is_some_and(|bundle| programs.bundles.holds(bundle)),
        );
    }

    /// The launcher opened, from its button or a lone tap of Meta: its shelf is
    /// ranked afresh, since use decays with the time passed, and the stores are
    /// asked to be re-read so an edit made through `applib`, or a fresh
    /// install, shows without restarting the session.
    ///
    /// The popup opens on the catalogue already in hand and adopts the fresh
    /// one when it lands: two documents and a manifest per installed program
    /// are far more than a frame's worth of reads.
    fn library_opened(
        memory: &LauncherMemory,
        catalogs: &Catalogs,
        (shell, compositor): (&mut DesktopShell, &mut Compositor),
        programs: &mut Programs,
    ) {
        shell.set_library_shelf(compositor, memory.shelf(launcher_now()));
        request_programs(catalogs, shell, compositor, programs);
    }

    /// Ask for a fresh program catalogue, adopting it at once when there is no
    /// scanner to read it elsewhere.
    fn request_programs(
        catalogs: &Catalogs,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        programs: &mut Programs,
    ) {
        if let Some(loaded) = catalogs.submit() {
            adopt_programs(loaded, shell, compositor, programs);
        }
    }

    /// How long the session holds its exit for the private records' last
    /// writes: a containment bound, so a store that stopped answering costs a
    /// moment of the logout rather than all of it.
    const SEE_OUT_NS: u64 = 2_000_000_000;

    /// Make the private records' last writes and wait — at most
    /// [`SEE_OUT_NS`] — for the store to take them, so a launch made or an
    /// icon put down just before the session ends is kept, saying so when one
    /// is not. What else the worker answers meanwhile belongs to a session
    /// that is over.
    fn see_out_private(memory: &mut PrivateRecords, worker: &PrivateWorker) {
        let deadline = tairix_rt::clock_get().saturating_add(SEE_OUT_NS);
        loop {
            if let Some(ask) = memory.launcher.next_store() {
                let number = ask.number;
                if worker
                    .submit(PrivateJob::LauncherStore {
                        number,
                        changes: ask.changes,
                    })
                    .is_err()
                {
                    // An answer collected below frees the room to ask again.
                    memory.launcher.unasked(number);
                }
            }
            if let Some(ask) = memory.arrangement.next_store() {
                let number = ask.number;
                if worker
                    .submit(PrivateJob::ArrangementStore {
                        number,
                        changes: ask.changes,
                    })
                    .is_err()
                {
                    memory.arrangement.unasked(number);
                }
            }
            if !memory.launcher.writing() && !memory.arrangement.writing() {
                if memory.launcher.holds_unconfirmed() {
                    app::report(
                        APP_NAME,
                        "the launcher's last changes were not kept: its record was never read",
                    );
                }
                if memory.arrangement.holds_unconfirmed() {
                    app::report(
                        APP_NAME,
                        "where the icons were last put was not kept: the record was never read",
                    );
                }
                return;
            }
            match worker.wait_until(deadline) {
                Some(PrivateAnswer::LauncherStored { number, result }) => {
                    if let Err(err) = &result {
                        app::report(
                            APP_NAME,
                            format_args!("the launcher's last changes were not kept ({err})"),
                        );
                    }
                    let _ = memory.launcher.stored(number, result);
                }
                Some(PrivateAnswer::ArrangementStored { number, result }) => {
                    if let Err(err) = &result {
                        app::report(
                            APP_NAME,
                            format_args!("where the icons were last put was not kept ({err})"),
                        );
                    }
                    let _ = memory.arrangement.stored(number, result);
                }
                Some(_) => {}
                None => {
                    app::report(
                        APP_NAME,
                        "the last changes to the launcher and the icons were not kept: the \
                         store did not answer in time",
                    );
                    return;
                }
            }
        }
    }

    /// The wall-clock second the launcher's record is kept against, or `None`
    /// while no wall time has been set this boot: a record dated by the epoch
    /// placeholder would decay to nothing the moment the clock is set.
    fn launcher_now() -> Option<i64> {
        let reading = tairix_rt::wall_time().ok()?;
        (reading.state() != WallTimeState::Unset).then(|| reading.time().secs())
    }

    /// Carry the changes made on the desktop into the record of where its
    /// icons stand, and hand the desktop the record whenever it moved: a
    /// change the record had no room for puts the icon back, and says so.
    fn arrangement_turn<S: DirectorySource>(
        arrangement: &mut ArrangementMemory,
        desktop: &mut Desktop<S>,
        windows: &mut SessionWindows,
        (shell, compositor): (&mut DesktopShell, &mut Compositor),
    ) {
        let mut cells = damage::sink();
        let full = desktop.keep_record(
            arrangement,
            |desk| shell.desktop_layout(compositor, desk),
            &mut cells,
        );
        let refusal = desktop.take_refusal().or_else(|| {
            full.then(|| {
                GestureRefusal::new(
                    "The desktop cannot keep where an icon was put",
                    "Its record of where icons stand is full or still waiting on the store, \
                     so the icon stands where it stood.",
                )
            })
        });
        if let Some(refusal) = refusal {
            state_refusal(&refusal, windows, shell, compositor);
        }
        if !cells.is_empty() {
            shell.present_desktop_area(compositor, desktop, &cells);
        }
    }

    /// Count the launches the user asked for this turn into the launcher's
    /// record, and hand the launcher its shelf whenever the record moved and
    /// what runs whenever the strip or the desktop's children did.
    fn launcher_turn(
        memory: &mut LauncherMemory,
        launched: &mut LaunchTable,
        apps: &mut AppBarPanel,
        (shell, compositor): (&mut DesktopShell, &mut Compositor),
    ) {
        let used = launched.take_used();
        if !used.is_empty() {
            let now = launcher_now();
            let catalog = shell.session().taskbar().library().catalog();
            let mut full = false;
            for entry in launched_entries(catalog, &used) {
                full |= memory.apply(Edit::Launched(entry, now)).is_err();
            }
            if full {
                app::report(
                    APP_NAME,
                    "the launcher's record has no room for another launch",
                );
            }
        }
        if memory.take_moved() {
            shell.set_library_shelf(compositor, memory.shelf(launcher_now()));
        }
        // Both latches drained, so neither survives a wake the other caused.
        let strip_moved = apps.take_moved();
        let children_moved = launched.take_moved();
        if strip_moved || children_moved {
            shell.set_library_running(compositor, running_bundles(&apps.strip, launched));
        }
    }

    /// Apply one shell outcome to the desktop's icon column and carry out
    /// whatever it asks for.
    ///
    /// The window manager reports a press or key that reached no window as one
    /// of the desktop outcomes, and those drive the column's selection,
    /// keyboard and activation. A pointer over an icon changes nothing about
    /// it, so motion matters only to a press carrying one, which every outcome
    /// follows first.
    ///
    /// A refusal — a file no installed application opens — is written to
    /// `stderr` and changes nothing else.
    #[allow(clippy::too_many_arguments)] // The desktop's whole mutable state, threaded explicitly.
    fn route_desktop<S: DirectorySource>(
        outcome: &Timed<tairix_desktop_session::ShellOutcome>,
        publisher: &Publisher,
        files: &Listings,
        pinboard: &mut PinboardPanel,
        wallpapers: &Wallpapers,
        desktop: &mut Desktop<S>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        windows: &SessionWindows,
        menu: &mut MenuChain,
        seat_modal: bool,
        launch: &mut LaunchCtx<'_>,
        programs: &mut Programs,
        now_ns: u64,
    ) -> Option<GestureRefusal> {
        let pointer = shell.router().pointer();
        let layout = shell.desktop_layout(compositor, desktop);
        // Every gesture reports the icon cells it changed here, and only
        // those are repainted: a click that moves focus between a window and
        // the desktop must cost a focus ring, not a screen.
        let mut damage = Region::new();
        // The desktop holds the keyboard exactly when no window does, so the
        // focus ring follows the window manager's one notion of focus rather
        // than a second one kept here.
        desktop.set_focused(shell.router().focused().is_none(), &layout, &mut damage);
        // A press on an icon carries it until the button comes up, and the
        // release may come with motion still to follow.
        let carried = desktop.follow_press(
            pointer,
            shell.router().holds_primary(),
            &layout,
            compositor.scale(),
            shell.session().active_theme(),
            &mut damage,
        );
        let acted = match &outcome.value {
            tairix_desktop_session::ShellOutcome::WindowManager(response) => match response {
                InputResponse::DesktopPressed => desktop.press(
                    pointer,
                    shell.modifiers(),
                    &layout,
                    compositor.scale(),
                    shell.session().active_theme(),
                    outcome.at_ns,
                    &programs.associations,
                    &mut damage,
                ),
                InputResponse::DesktopSecondaryPressed => {
                    // The backdrop menu is the seat's one chain, so this asks
                    // for it directly rather than naming an action: it is the
                    // desktop's own model handed to the one service, exactly
                    // as an application's `OpenMenu` is.
                    let on_icon = desktop.context_press(
                        pointer,
                        &layout,
                        compositor.scale(),
                        shell.session().active_theme(),
                        &mut damage,
                    );
                    open_backdrop_menu(
                        pointer, on_icon, seat_modal, desktop, menu, shell, compositor, windows,
                    );
                    DesktopOutcome::ignored()
                }
                InputResponse::DesktopKey { key, pressed, .. } => {
                    desktop.key(*key, *pressed, &layout, &programs.associations, &mut damage)
                }
                _ => DesktopOutcome::ignored(),
            },
            _ => DesktopOutcome::ignored(),
        };
        // A shortcut the program library's row menu asked for is a change to
        // *this* folder, so it is honoured beside the desktop's own gestures
        // rather than through a second path. The sources are exclusive — a
        // taskbar outcome is never also a desktop gesture — and `or` says so
        // without discarding either.
        let action = shortcut_asked(&outcome.value, shell, desktop)
            .or(acted.action)
            .or(carried.action);
        // The settings and folder edits `apply_desktop_action` performs are
        // the changes that genuinely repaint the whole layer.
        let applied = apply_desktop_action(
            action, publisher, files, pinboard, wallpapers, desktop, shell, compositor, launch,
            now_ns,
        );
        show_desktop_change(applied, &damage, desktop, shell, compositor)
    }

    /// Show what a desktop gesture changed — the whole layer where the action
    /// it asked for rearranged it, else the icons `damage` marks — answering
    /// the refusal the action met.
    fn show_desktop_change<S: DirectorySource>(
        applied: Result<bool, GestureRefusal>,
        damage: &Region,
        desktop: &Desktop<S>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) -> Option<GestureRefusal> {
        if applied == Ok(true) {
            shell.present_desktop(compositor, desktop);
        } else if !damage.is_empty() {
            shell.present_desktop_area(compositor, desktop, damage);
        }
        applied.err()
    }

    /// Resolve the desktop layer surface's feeds for this frame: hide or show
    /// it as a trusted surface comes and goes, notice whether the desktop's
    /// shape changed, and deliver at most one message of each kind.
    ///
    /// Every open, refusal, and retirement of a layer surface is a security
    /// decision recorded elsewhere; what is recorded *here* is the state edge
    /// that starts and stops the pointer feed, because that is the moment the
    /// holder's view of the screen changes.
    fn serve_layer_feeds(
        shell: &DesktopShell,
        windows: &mut SessionWindows,
        compositor: &mut Compositor,
        server: &mut WindowServer<RtShmMapper>,
        sink: &mut RtEventSink,
        trusted_up: bool,
    ) {
        if windows.layers.set_suppressed(trusted_up, compositor) {
            let message = if trusted_up {
                LAYER_FEEDS_STOPPED_MESSAGE
            } else {
                LAYER_FEEDS_RESUMED_MESSAGE
            };
            log_info(LAYER_FEEDS, message, &[]);
        }
        // Every open, refusal, and retirement is a security decision: the
        // holder gains — or is denied — presence on the desktop and a feed of
        // the pointer's position across the whole screen.
        windows.layers.report_decisions(
            |decision| {
                let (message, reason) = match decision {
                    LayerDecision::Opened => (LAYER_OPENED_MESSAGE, None),
                    LayerDecision::Refused(err) => (LAYER_REFUSED_MESSAGE, Some(err)),
                    LayerDecision::Retired => (LAYER_RETIRED_MESSAGE, None),
                };
                let id = match decision {
                    LayerDecision::Opened => LAYER_OPENED,
                    LayerDecision::Refused(_) => LAYER_REFUSED,
                    LayerDecision::Retired => LAYER_RETIRED,
                };
                let fields = match reason {
                    Some(err) => &[LogField {
                        key: "reason",
                        value: LogFieldValue::SignedInt(i64::from(err.as_i32())),
                    }][..],
                    None => &[][..],
                };
                log_info(id, message, fields);
            },
            |dropped| {
                log(
                    &LOG_SINK,
                    &LogEvent {
                        level: LogLevel::Warn,
                        id: LAYER_REFUSED,
                        message: LAYER_REFUSED_MESSAGE,
                        fields: &[LogField {
                            key: "unrecorded",
                            value: LogFieldValue::UnsignedInt(u64::from(dropped)),
                        }],
                    },
                );
            },
        );
        let Some(surface) = windows.layers.surface() else {
            return;
        };
        if !windows.layers.is_suppressed() {
            windows.layers.observe_terrain(compositor, surface.wm);
            // Sampled once a frame from the tracked pointer rather than
            // hooked onto each input path: that is the coalescing the feed
            // wants anyway, and it cannot miss a path that moves the pointer.
            windows.layers.pointer_moved(shell.router().pointer());
        }
        // Collected before delivering: `take_feeds` borrows the state, and a
        // delivery may tear the surface down when its owner has gone.
        let mut pending = [None, None];
        for (slot, feed) in pending.iter_mut().zip(windows.layers.take_feeds()) {
            *slot = Some(feed);
        }
        for feed in pending.into_iter().flatten() {
            let event = match feed {
                LayerFeed::Terrain { ipc, generation } => WindowEvent::TerrainChanged {
                    window_id: ipc,
                    generation,
                },
                LayerFeed::Pointer { ipc, x, y } => WindowEvent::LayerPointer {
                    window_id: ipc,
                    x,
                    y,
                },
            };
            // A refused send is the sink's to hold; a vanished owner is torn
            // down on the next window-scoped delivery like any other.
            let _ = server.deliver_event(sink, &event);
        }
    }

    /// Open the backdrop menu as the seat's one chain, anchored at the press
    /// that asked for it.
    ///
    /// The desktop is a client of the menu service like any application; its
    /// model is built here rather than decoded from the wire, so its rows may
    /// state what only the system can (a command it lacks the authority for).
    /// A model the chain will not show is reported and opens nothing.
    #[allow(clippy::too_many_arguments)] // The chain's whole mutable surround, threaded explicitly.
    fn open_backdrop_menu<S: DirectorySource>(
        at: Point,
        on_icon: bool,
        seat_modal: bool,
        desktop: &Desktop<S>,
        menu: &mut MenuChain,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        windows: &SessionWindows,
    ) {
        let model = desktop.menu(on_icon);
        let geom = chain_geometry(shell.session(), compositor);
        match open_desktop_menu(
            menu,
            ChainOwner::Backdrop,
            model,
            window_menu_placement(Rect::new(at.x, at.y, 0, 0)),
            seat_modal,
            &geom,
        ) {
            Ok(()) => present_menu_chain(menu, shell, compositor, windows),
            Err(refused) => {
                app::report(APP_NAME, format_args!("no backdrop menu ({refused:?})"));
            }
        }
    }

    /// The action a program-library *Create Desktop Shortcut* row asks for,
    /// or `None` for every other outcome.
    ///
    /// The catalog the popup was handed is the one the shortcut is resolved
    /// against, so a row can never launch one bundle and link another.
    fn shortcut_asked<S: DirectorySource>(
        outcome: &tairix_desktop_session::ShellOutcome,
        shell: &DesktopShell,
        desktop: &Desktop<S>,
    ) -> Option<DesktopAction> {
        let tairix_desktop_session::ShellOutcome::Taskbar(TaskbarResponse::CreateDesktopShortcut {
            entry,
        }) = outcome
        else {
            return None;
        };
        Some(desktop.shortcut_to(shell.session().taskbar().library().catalog(), entry))
    }

    /// Carry out one desktop action, whether a gesture on the icon column
    /// named it or a chosen backdrop-menu row did, and answer whether the
    /// whole desktop layer must be repainted.
    ///
    /// The single place every [`DesktopAction`] is honoured, so a
    /// double-click and the equivalent menu row can never disagree about
    /// what happens. Every failure — a refused launch, a folder the
    /// filesystem would not create, settings that could not be saved — is
    /// stated on `stderr` and leaves the desktop running.
    #[allow(clippy::too_many_arguments)] // The desktop's whole mutable state, threaded explicitly.
    fn apply_desktop_action<S: DirectorySource>(
        action: Option<DesktopAction>,
        publisher: &Publisher,
        files: &Listings,
        pinboard: &mut PinboardPanel,
        wallpapers: &Wallpapers,
        desktop: &mut Desktop<S>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        launch: &mut LaunchCtx<'_>,
        now_ns: u64,
    ) -> Result<bool, GestureRefusal> {
        Ok(match action {
            // The desktop's file manager is handed the folder; one that has
            // gone is started again to show it.
            Some(DesktopAction::Activate(DesktopActivation::OpenFolder { path })) => {
                launch.hand_file_manager(
                    (shell, compositor),
                    &[
                        tairix_window::DESKTOP_ROLE_SWITCH.as_bytes(),
                        path.as_bytes(),
                    ],
                    LaunchTarget::Path(&path),
                );
                false
            }
            Some(DesktopAction::Activate(DesktopActivation::Launch {
                run_path,
                label,
                document: None,
            })) => {
                let _ = launch.launch(shell, compositor, &run_path, &label, &[], None);
                false
            }
            // The file manager attaches it, as an image opened in one of its
            // windows is, so it is the file manager's to let go once shown.
            Some(DesktopAction::Activate(DesktopActivation::MountImage { path })) => {
                launch.hand_file_manager(
                    (shell, compositor),
                    &[
                        tairix_window::DESKTOP_ROLE_SWITCH.as_bytes(),
                        tairix_browse::mount::MOUNT_SWITCH.as_bytes(),
                        path.as_bytes(),
                    ],
                    LaunchTarget::Image(&path),
                );
                false
            }
            // The document is opened on the file worker, and the launch
            // carried out once it has been.
            Some(DesktopAction::Activate(DesktopActivation::Launch {
                run_path,
                label,
                document: Some(document),
            })) => {
                let call = DesktopCall::Document {
                    run_path,
                    label,
                    document,
                };
                ask_for_desktop_call(call, files, desktop, shell, compositor, launch);
                false
            }
            Some(DesktopAction::CreateFolder { path }) => {
                let call = DesktopCall::Folder { path };
                ask_for_desktop_call(call, files, desktop, shell, compositor, launch);
                false
            }
            Some(DesktopAction::CreateShortcut { link, target }) => {
                let call = DesktopCall::Shortcut { link, target };
                ask_for_desktop_call(call, files, desktop, shell, compositor, launch);
                false
            }
            Some(DesktopAction::EditSettings(edit)) => request_pinboard_settings(
                edit.applied_to(publisher.base(desktop.settings())),
                publisher,
                None,
                pinboard,
                wallpapers,
                desktop,
                shell,
                compositor,
                now_ns,
            ),
            // Wallpaper is a section of Settings, not an application beside
            // it. A Settings already running is handed the pane and
            // navigates; a fresh one is given the same pane as its argument
            // and opens on it.
            Some(DesktopAction::ChangeBackground) => {
                let _ = launch.launch(
                    shell,
                    compositor,
                    SETTINGS_RUN_PATH,
                    SETTINGS_LABEL,
                    &[tairix_wallpaper::WALLPAPER_PANE.as_bytes()],
                    Some(LaunchTarget::Pane(tairix_wallpaper::WALLPAPER_PANE)),
                );
                false
            }
            Some(DesktopAction::Volume(act)) => {
                let call = DesktopCall::Volume(act);
                ask_for_desktop_call(call, files, desktop, shell, compositor, launch);
                false
            }
            Some(DesktopAction::Refuse(refusal)) => return Err(refusal),
            None => false,
        })
    }

    /// The thread that reads which volumes await the seat's answer for their
    /// held writes and carries each answer to the kernel, in the order asked.
    type HeldWorker =
        Worker<RtHeld, HeldJob, HeldDone, tairix_util::defer::JobQueue<HeldJob, HeldDone>>;

    /// The held-writes worker's reach: the mount table, and the kernel's
    /// answer call.
    struct RtHeld;

    impl HeldSystem for RtHeld {
        fn mounts(
            &mut self,
            sink: &mut dyn FnMut(&tairix_abi::sysinfo::MountRecord),
        ) -> Result<(), Errno> {
            use tairix_procinfo::{CallError, ListError};
            tairix_procinfo::for_each_mount(&IpcTransport, |record| {
                sink(record);
                Ok(tairix_procinfo::WalkStep::Continue)
            })
            .map_err(|err| match err {
                ListError::Call(CallError::PermissionDenied) => Errno::PermissionDenied,
                ListError::Call(CallError::Service(errno)) | ListError::Sink(errno) => errno,
            })
        }

        fn replay(
            &mut self,
            request: &tairix_abi::volume::VolumeReplayRequest,
        ) -> Result<(), Errno> {
            let mut frame = [0u8; tairix_abi::volume::VOLUME_REPLAY_LEN];
            let len = request.encode(&mut frame)?;
            Errno::syscall_result(tairix_rt::volume_replay(&frame[..len])).map(|_| ())
        }
    }

    /// Carry each answer the user gave for held writes to the worker with a
    /// read of the mount table owed behind them — and behind a move of the
    /// table, `moved` — then adopt what the worker did.
    fn serve_held(
        (worker, questions): (&HeldWorker, &mut HeldQuestions),
        moved: bool,
        windows: &mut SessionWindows,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        let mut submit = |job| worker.submit(job).map(|_| ());
        if moved {
            questions.moved();
        }
        windows
            .alerts
            .relay_held(|answered| questions.carry(answered, &mut submit));
        questions.read(&mut submit);
        // A job carried out here for want of a worker has landed already.
        while let Some(done) = worker.collect() {
            let parts = (&mut windows.alerts, &mut windows.holds);
            match questions.adopt(done, &mut submit, parts, shell, compositor) {
                HeldWord::Nothing => {}
                HeldWord::Note(note) => app::report(APP_NAME, note),
                HeldWord::Refusal(refusal) => state_refusal(&refusal, windows, shell, compositor),
            }
        }
    }

    /// State a refused desktop gesture: on `stderr` for the record, and in a
    /// notice of the desktop's own for the user who made it.
    fn state_refusal(
        refusal: &GestureRefusal,
        windows: &mut SessionWindows,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        app::report_line(&refusal.line());
        let Ok(spec) =
            AlertSpec::acknowledged(AlertSeverity::Error, refusal.headline(), refusal.detail())
        else {
            return;
        };
        let title = alert_title(AlertSeverity::Error, Speaker::Desktop);
        if !windows
            .alerts
            .notify(&spec, &title, &mut windows.holds, shell, compositor)
        {
            app::report(APP_NAME, format_args!("no window to state a refusal in"));
        }
    }

    /// State that an application does not take what was dropped on its slot,
    /// in that application's name.
    fn state_drop_refusal(
        refusal: &DropRefusal,
        windows: &mut SessionWindows,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        let Some(spec) = refusal.spec() else {
            return;
        };
        let title = alert_title(AlertSeverity::Error, Speaker::App(refusal.app()));
        if !windows
            .alerts
            .notify(&spec, &title, &mut windows.holds, shell, compositor)
        {
            app::report(
                APP_NAME,
                format_args!("no window to state a refused drop in"),
            );
        }
    }

    /// Settle a name the session asked the filesystem to create at `path`,
    /// whose call answered `made`, and show the result: through the folder's
    /// watch when it follows the folder, otherwise by re-listing it and
    /// repainting only the icon cells the fresh listing changed.
    ///
    /// Both names the desktop creates — a folder and a shortcut — end here,
    /// so a refusal reads the same whichever asked for it and the fresh name
    /// appears the same way. A refusal (the name is already taken, the
    /// desktop folder is not writable, the volume is full) is stated on
    /// `stderr` with the kernel's own reason and leaves the desktop exactly
    /// as it was.
    fn settle_desktop_create<S: DirectorySource>(
        path: &str,
        made: Result<(), Errno>,
        desktop: &mut Desktop<S>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        if let Err(err) = made {
            desktop.refuse(GestureRefusal::unmade(path, err));
            return;
        }
        show_unreported(Some(path), desktop, shell, compositor);
    }

    /// Show what the session knows changed in the desktop's folder that no
    /// watch reports — the name made at `made`, or with none whatever the
    /// mount table moving changed — reading the folder again where its
    /// listing does not follow it, and repainting only the icon cells the
    /// fresh listing moved.
    fn show_unreported<S: DirectorySource>(
        made: Option<&str>,
        desktop: &mut Desktop<S>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        let layout = |desk: &Desktop<S>| shell.desktop_layout(compositor, desk);
        let mut cells = damage::sink();
        let moved = if let Some(path) = made {
            desktop.show_made(path, layout, &mut cells)
        } else {
            desktop.relist_unfollowed(layout, &mut cells)
        };
        if moved {
            shell.present_desktop_area(compositor, desktop, &cells);
        }
    }

    /// Ask the file worker for `call`, settling it here when it was answered
    /// at once — carried out on this thread for want of a worker, or refused
    /// because too many are waiting.
    fn ask_for_desktop_call<S: DirectorySource>(
        call: DesktopCall,
        files: &Listings,
        desktop: &mut Desktop<S>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        launch: &mut LaunchCtx<'_>,
    ) {
        // An answer is for the call it was asked with.
        if let Some(FileAnswer::Desktop(done)) = files.submit(FileJob::Desktop(call)) {
            settle_desktop_call(done, desktop, shell, compositor, launch);
        }
    }

    /// Settle a desktop call the file worker carried out.
    ///
    /// A document launches its application with the descriptor opened for it;
    /// a folder or shortcut made is shown; a file placed is handed to the
    /// window it was dropped on, or why it could not be is held for the desktop
    /// to state. Any other refusal is stated on `stderr` with the kernel's own
    /// reason and leaves the desktop as it was.
    fn settle_desktop_call<S: DirectorySource>(
        done: DesktopAnswer,
        desktop: &mut Desktop<S>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        launch: &mut LaunchCtx<'_>,
    ) {
        match done {
            DesktopAnswer::Document {
                run_path,
                label,
                document,
                opened,
            } => {
                let name = tairix_browse::leaf_name(&document.path);
                match opened {
                    Ok(opened) => {
                        let _ = launch
                            .launch_document(shell, compositor, &run_path, &label, name, &opened);
                    }
                    Err(err) => desktop.refuse(GestureRefusal::unopened(name, err)),
                }
            }
            DesktopAnswer::Placed { placed, opened } => {
                let handed = opened
                    .map_err(PlaceRefused::Unopened)
                    .and_then(|opened| launch.place_document(&placed, &opened));
                if let Err(why) = handed {
                    desktop.refuse(placed.refusal(why));
                }
            }
            DesktopAnswer::Made { path, made } => {
                settle_desktop_create(&path, made, desktop, shell, compositor);
            }
            DesktopAnswer::Mounts(read) => settle_mounts(read, desktop, shell, compositor),
            DesktopAnswer::MountsRefused => desktop.mount_read_refused(),
            DesktopAnswer::Volume(done) => desktop.volume_done(done),
        }
    }

    /// Adopt a read of the mount table, repainting only the desktop icons it
    /// changed.
    fn settle_mounts<S: DirectorySource>(
        read: Option<Vec<tairix_browse::Volume>>,
        desktop: &mut Desktop<S>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        let layout = |desk: &Desktop<S>| shell.desktop_layout(compositor, desk);
        let mut cells = damage::sink();
        if desktop.adopt_mounts(read, layout, &mut cells) {
            shell.present_desktop_area(compositor, desktop, &cells);
        }
    }

    /// Ask the file worker for a read of the mount table where one is owed and
    /// none is under way, adopting it here when it was answered at once.
    fn ask_for_mounts<S: DirectorySource>(
        files: &Listings,
        desktop: &mut Desktop<S>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        if !desktop.take_mount_read() {
            return;
        }
        match files.submit(FileJob::Desktop(DesktopCall::Mounts)) {
            Some(FileAnswer::Desktop(DesktopAnswer::Mounts(read))) => {
                settle_mounts(read, desktop, shell, compositor);
            }
            Some(_) => desktop.mount_read_refused(),
            None => {}
        }
    }

    /// Ask for `settings` to be published, and adopt them if the answer comes
    /// back on this thread.
    ///
    /// Every change to the desktop's own settings — a chosen menu row, an
    /// apply from Settings, a kept resolution, a remembered sound control —
    /// comes through here, and a files apply submits to the same publisher, so
    /// none can adopt something another would not have. Nothing is adopted at
    /// the point of asking: the store round trip happens on the settings
    /// worker, and the answer is adopted by [`collect_publish`] on the wake it
    /// nudges. With no worker to answer it the publish happens here and is
    /// adopted at once.
    ///
    /// Answers whether the desktop layer needs a whole repaint.
    #[allow(clippy::too_many_arguments)] // The desktop's whole settings state, threaded explicitly.
    fn request_pinboard_settings<S: DirectorySource>(
        settings: DesktopSettings,
        publisher: &Publisher,
        ticket: Option<u64>,
        pinboard: &mut PinboardPanel,
        wallpapers: &Wallpapers,
        desktop: &mut Desktop<S>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        now_ns: u64,
    ) -> bool {
        let files = publisher.files_base(shell.files());
        publisher
            .submit(settings, files, ticket)
            .is_some_and(|answer| {
                adopt_publish(
                    answer, publisher, pinboard, wallpapers, desktop, shell, compositor, now_ns,
                )
            })
    }

    /// Adopt whatever the settings worker has published, if anything, and do
    /// exactly the work the resulting change names: re-lay-out the icons and
    /// re-prepare the wallpaper.
    ///
    /// Answers whether the desktop layer needs a whole repaint.
    fn collect_publish<S: DirectorySource>(
        publisher: &Publisher,
        pinboard: &mut PinboardPanel,
        wallpapers: &Wallpapers,
        desktop: &mut Desktop<S>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        now_ns: u64,
    ) -> bool {
        publisher.collect().is_some_and(|answer| {
            adopt_publish(
                answer, publisher, pinboard, wallpapers, desktop, shell, compositor, now_ns,
            )
        })
    }

    /// Adopt one landed publish: answer the call that asked for it, then apply
    /// the settings the store now holds.
    ///
    /// The write came first, so memory and disk can never diverge: a refused
    /// write states why on `stderr`, adopts nothing, and leaves the desktop
    /// showing the settings the next login would restore. An ask made over
    /// the defaults standing in for an unread store published nothing: what
    /// the store was read to hold is adopted in its place, and the ask refused.
    #[allow(clippy::too_many_arguments)] // The desktop's whole settings state, threaded explicitly.
    fn adopt_publish<S: DirectorySource>(
        answer: PublishAnswer,
        publisher: &Publisher,
        pinboard: &mut PinboardPanel,
        wallpapers: &Wallpapers,
        desktop: &mut Desktop<S>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        now_ns: u64,
    ) -> bool {
        if let Some(ticket) = answer.ticket {
            publisher.answer(ticket, answer.reply());
        }
        publisher.landed(answer.number);
        let landed = match answer.outcome {
            Ok(Publication::Published(landed)) => landed,
            Ok(Publication::Read(read)) => {
                publisher.read_store();
                app::report(
                    APP_NAME,
                    "a change made before the desktop settings could be read was not \
                     published; they are read now",
                );
                read
            }
            Err(err) => {
                app::report(
                    APP_NAME,
                    format_args!("the desktop settings could not be published ({err:?})"),
                );
                return false;
            }
        };
        for warning in &landed.warnings {
            app::report_line(warning);
        }
        shell.adopt_published_files(landed.files);
        let relabelled = desktop.hide_endings(landed.files.presentation.endings)
            | desktop.show_hidden_items(landed.files.presentation.hidden_items)
            | desktop.show_drives(landed.files.desktop);
        publish_desktop(compositor, shell);
        let wanted = landed.settings.clone();
        let Some(change) = desktop.apply_settings(landed.settings) else {
            return relabelled;
        };
        let papered = change.backdrop.wallpaper
            && prepare_wallpaper(pinboard, wallpapers, shell, desktop, compositor, now_ns);
        adopt_appearance(change.appearance, &wanted, shell, compositor);
        if change.notifications {
            shell.withdraw_unadmitted(compositor, &wanted.notifications);
        }
        if change.clock {
            let reshaped = shell.adopt_civil(&wanted).reshaped;
            if !tick_clock(shell, compositor, now_ns) && reshaped {
                shell.present(compositor);
            }
        }
        change.layer || papered || relabelled
    }

    /// Put the *appearance* half of a settings change into effect: the theme
    /// axes and the UI scale.
    ///
    /// The one place they are adopted, so the desktop a user sees after a
    /// login and the desktop they see after a change made while logged in
    /// cannot differ. The backdrop half of the same change — the wallpaper,
    /// the icon flow, the sort order — is the desktop model's and is applied
    /// by its own caller; this touches only what every surface on the screen
    /// is drawn with.
    fn adopt_appearance(
        change: AppearanceWork,
        settings: &DesktopSettings,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        if change.theme {
            shell.session_mut().set_appearance(settings.appearance);
            shell.session_mut().set_accessibility(Accessibility {
                contrast: settings.contrast,
                density: settings.density,
                motion: settings.motion,
            });
            if let Some(gone) = shell
                .session_mut()
                .set_text(settings.text_family, settings.text_size)
            {
                app::report(
                    APP_NAME,
                    format_args!(
                        "the font family `{}` is not installed; the theme's own is drawn",
                        gone.as_str()
                    ),
                );
            }
            shell.sync_theme(compositor);
            shell.present(compositor);
        }
        if change.scale {
            shell.set_scale(settings.scale, compositor);
        }
        if change.cursor {
            shell.set_cursor_look(
                settings.cursor_set,
                settings.cursor_size,
                settings.cursor_shadow,
                compositor,
            );
        }
        if change.any() {
            // Every served window holds its application's own pixels, which
            // the session cannot redraw: it says how the desktop now looks
            // and each application repaints itself. Without this the desktop
            // would change and every open window would sit there in the
            // appearance and density the user just left.
            publish_desktop(compositor, shell);
        }
    }

    /// Answer a `PINBOARD_ENDPOINT` call with the shared status frame.
    ///
    /// The one place a pinboard call is answered, so every route — a landed
    /// publish, a refusal the store gave, a request the next gesture overtook,
    /// and a refusal the attestation itself produced — words its reply
    /// identically.
    fn reply_pinboard(ticket: u64, outcome: Result<(), Errno>) {
        let reply = encode_status_reply(outcome);
        reply_call(PINBOARD_ENDPOINT, ticket, &reply);
    }

    /// Answer `ticket`, a call received on `endpoint`, with `reply`. The reply
    /// lands in the room its call was posted with, so the kernel refuses it
    /// only for a call its caller has withdrawn, which nobody waits on; any
    /// other refusal is the session's own defect, and is said so.
    fn reply_call(endpoint: u64, ticket: u64, reply: &[u8]) {
        let ret = tairix_rt::call_reply(endpoint, ticket, reply);
        if ret != 0 && Errno::from_syscall(ret) != Errno::NotFound {
            io::write_stderr_line("desktop: the kernel refused a reply its caller waits on");
        }
    }

    /// Send the present answers `server` has due: after a latch, after a
    /// serve, and before every park.
    fn answer_presents(server: &mut WindowServer<RtShmMapper>) {
        server.send_answers(|ticket, reply| reply_call(WINDOW_ENDPOINT, ticket, reply));
    }

    /// The compositor window of the first served window owned by `app`,
    /// resolved through the window engine's attested ownership records —
    /// never a window title or any other app-controlled data.
    fn window_of_app(
        app: ProcId,
        server: &WindowServer<RtShmMapper>,
        windows: &SessionWindows,
    ) -> Option<tairix_wm::WindowId> {
        windows
            .served()
            .find_map(|(ipc, wm)| (server.owner_of(ipc)? == app).then_some(wm))
    }

    /// A window whose owner the kernel attests runs the bundle installed at
    /// `bundle`, whoever started it.
    fn window_of_bundle(
        bundle: &str,
        server: &WindowServer<RtShmMapper>,
        windows: &SessionWindows,
        identity: &RtWindowIdentity,
        index: &BundleIndex,
    ) -> Option<tairix_wm::WindowId> {
        windows.served().find_map(|(ipc, wm)| {
            let app = identity.app_of(server.owner_of(ipc)?)?;
            (index.path_of(&app)? == bundle).then_some(wm)
        })
    }

    /// Resolve a program-library launch: the chosen entry's bundle names its
    /// `Run` binary; spawn it and record the launch under the entry's
    /// display name.
    fn launch_library_entry(
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        entry: &tairix_proglib::EntryId,
        launch: &mut LaunchCtx<'_>,
    ) {
        let catalog = shell.session().taskbar().library().catalog();
        let chosen = match catalogued(catalog, entry) {
            Ok(chosen) => chosen,
            Err(reason) => {
                app::report_line(&reason);
                return;
            }
        };
        let run_path = tairix_appstore::entry_path(chosen.bundle().as_str());
        // Owned before the launch, which needs the shell mutably: the chosen
        // entry borrows the catalog the shell holds.
        let label = alloc::string::String::from(chosen.name().as_str());
        let _ = launch.launch(shell, compositor, &run_path, &label, &[], None);
    }

    /// The session's live file-reading seam: whole-file reads through the
    /// kernel VFS under the session's own kernel-attested identity, each
    /// bounded by the format its caller names.
    struct VfsFileReader;

    impl SessionFileReader for VfsFileReader {
        fn read(&mut self, path: &str, max: usize) -> Result<alloc::vec::Vec<u8>, Errno> {
            read_file(path, max)
        }

        fn open_document(
            &mut self,
            path: &str,
        ) -> Option<alloc::boxed::Box<dyn tairix_icon::ArtworkDocument + '_>> {
            tairix_icon::RtDocument::open(path).map(|document| {
                alloc::boxed::Box::new(document)
                    as alloc::boxed::Box<dyn tairix_icon::ArtworkDocument>
            })
        }

        fn open_document_inside(
            &mut self,
            dir: &str,
            inside: &str,
        ) -> Option<alloc::boxed::Box<dyn tairix_icon::ArtworkDocument + '_>> {
            tairix_icon::RtDocument::open_inside(dir, inside).map(|document| {
                alloc::boxed::Box::new(document)
                    as alloc::boxed::Box<dyn tairix_icon::ArtworkDocument>
            })
        }
    }

    impl tairix_appstore::StoreReader for VfsFileReader {
        fn list_dir(
            &self,
            path: &str,
        ) -> Result<Option<alloc::vec::Vec<tairix_appstore::DirEntry>>, Errno> {
            let stream = match tairix_rt::read_dir_all(path.as_bytes()) {
                Ok(stream) => stream,
                // A store root this installation does not have is ordinary;
                // anything else surfaces and the walk fails closed.
                Err(ret) => {
                    let err = Errno::from_syscall(ret);
                    return if err == Errno::NotFound {
                        Ok(None)
                    } else {
                        Err(err)
                    };
                }
            };
            let entries =
                tairix_browse::vfs::entries_from_dir_stream(path, &stream, &mut RtLinkReader)
                    .map_err(|_| Errno::OutOfRange)?;
            Ok(Some(
                entries
                    .iter()
                    .map(|entry| tairix_appstore::DirEntry {
                        name: alloc::string::String::from(entry.name()),
                        directory: entry.is_directory_backed(),
                    })
                    .collect(),
            ))
        }

        fn read_appinfo(&self, bundle: &str) -> Result<Option<alloc::vec::Vec<u8>>, Errno> {
            match read_file(
                &tairix_appstore::manifest_path(bundle),
                tairix_abi::APPINFO_WIRE_MAX,
            ) {
                Ok(bytes) => Ok(Some(bytes)),
                // A directory with no manifest is simply not a bundle.
                Err(Errno::NotFound) => Ok(None),
                Err(err) => Err(err),
            }
        }
    }

    /// The logged-in account's home directory, as the session inherited it.
    ///
    /// The account's own two program stores are resolved against it, so a
    /// session with no usable `HOME` simply has none — every machine-wide
    /// store is still walked.
    fn home_dir() -> Option<alloc::string::String> {
        let home = tairix_rt::env_var(b"HOME")?;
        core::str::from_utf8(home)
            .ok()
            .map(alloc::string::String::from)
    }
    /// Read the whole file at `path` through the kernel VFS under the
    /// session's own kernel-attested identity, answering at most one byte past
    /// `cap` so no file can make the desktop slurp an arbitrary number of bytes.
    ///
    /// The one read path every file the session reads goes through — the
    /// machine-wide program-library store at the configuration-document cap,
    /// the user's wallpaper at the wallpaper cap — so a second,
    /// differently-bounded reader cannot exist. An answer longer than `cap` is the caller's
    /// whole-document refusal to state.
    ///
    /// The streaming is the runtime's one whole-file policy
    /// ([`tairix_rt::read_fd_to_end`]), so the desktop cannot drift to a chunk
    /// size of its own: a wallpaper master is megabytes, and reading one a
    /// kilobyte per syscall spent thousands of traps — seconds of them on real
    /// storage — before the decoder saw a byte.
    fn read_file(path: &str, cap: usize) -> Result<alloc::vec::Vec<u8>, Errno> {
        tairix_rt::read_path_to_end(path.as_bytes(), cap).map_err(Errno::from_syscall)
    }

    /// Walk the read-only shipped wallpaper store and build the one flat
    /// catalog the desktop offers a browsing application.
    ///
    /// Done once, at bring-up: `/System` is mounted read-only, so the store
    /// cannot change under a running session and the answer is fixed for
    /// the life of the boot. That is what lets the catalog query be served
    /// from memory, with no directory walk anywhere near the compositing
    /// loop.
    ///
    /// A store that cannot be listed is not fatal — a desktop simply offers
    /// no shipped pictures — and one unreadable category costs only its own
    /// wallpapers. Every refusal is stated on `stderr`.
    fn list_wallpaper_store() -> Vec<WallpaperName> {
        let Some(entries) = list_store_dir(WALLPAPER_STORE, "wallpapers") else {
            return Vec::new();
        };
        // The store's own children are the categories; a stray file there is
        // planted by nothing and offered by nothing.
        let categories = tairix_wallpaper::catalog_categories(
            entries
                .iter()
                .filter(|entry| entry.is_directory_backed())
                .map(tairix_browse::Entry::name),
        );
        let mut listings: Vec<(alloc::string::String, Vec<tairix_wallpaper::CatalogEntry>)> =
            Vec::with_capacity(categories.len());
        for category in categories {
            let Some(listing) =
                list_store_dir(&tairix_wallpaper::category_path(&category), "wallpapers")
            else {
                continue;
            };
            // The shared catalog builder decides what counts as a wallpaper
            // (name shape, extension, ordering, and the listing bound); this
            // only drops the directories, which are never candidates.
            let entries = tairix_wallpaper::catalog_entries(
                listing
                    .iter()
                    .filter(|entry| !entry.is_directory_backed())
                    .map(|entry| {
                        (
                            entry.name(),
                            usize::try_from(entry.size()).unwrap_or(usize::MAX),
                        )
                    }),
            );
            listings.push((category, entries));
        }
        tairix_wallpaper::desktop_catalog(
            listings
                .iter()
                .map(|(category, entries)| (category.as_str(), entries.as_slice())),
        )
        .into_iter()
        .map(|item| WallpaperName {
            category: item.category,
            file: item.file,
        })
        .collect()
    }

    /// Walk the read-only shipped cursor store and load every set it
    /// offers, each under its own directory name.
    ///
    /// Done once, at bring-up, for the reason the wallpaper walk is:
    /// `/System` is mounted read-only, so the choice space is fixed for the
    /// life of the boot. Every set's nine assets are read here too, so
    /// *activating* a set later is pure memory — the choice arrives on the
    /// loop that owes the user a frame, and reading a directory there is
    /// exactly what the desktop must never do.
    ///
    /// A store that cannot be listed is not fatal — the desktop then offers
    /// only the built-in set — and one unreadable asset costs only its own
    /// kind, which keeps its built-in cursor.
    fn load_cursor_sets(
        shell: &DesktopShell,
    ) -> Vec<(tairix_theme::CursorSetId, tairix_cursor::CursorTheme)> {
        let Some(entries) = list_store_dir(tairix_cursor::CURSOR_STORE, "pointer sets") else {
            return Vec::new();
        };
        // The store's own children are the sets; a stray file there is
        // planted by nothing and offered by nothing.
        let sets = tairix_cursor::catalog_sets(
            entries
                .iter()
                .filter(|entry| entry.is_directory_backed())
                .map(tairix_browse::Entry::name),
        );
        sets.into_iter()
            .map(|set| {
                let theme = shell.session().load_cursors(
                    &mut VfsFileReader,
                    set,
                    &mut tairix_font::ServiceFonts::new(),
                );
                (set, theme)
            })
            .collect()
    }

    /// One shipped store directory's entries, or `None` with the reason
    /// stated on `stderr`, naming the `offered` it holds.
    fn list_store_dir(path: &str, offered: &str) -> Option<Vec<Entry>> {
        let stream = match tairix_rt::read_dir_all(path.as_bytes()) {
            Ok(stream) => stream,
            Err(ret) => {
                app::report(
                    APP_NAME,
                    format_args!(
                        "{path}: {}; its {offered} are not offered",
                        Errno::from_syscall(ret)
                    ),
                );
                return None;
            }
        };
        let Ok(entries) =
            tairix_browse::vfs::entries_from_dir_stream(path, &stream, &mut RtLinkReader)
        else {
            app::report(
                APP_NAME,
                format_args!("{path}: listing not readable; its {offered} are not offered"),
            );
            return None;
        };
        Some(entries)
    }

    /// The sound themes the shipped store holds, in name order: read once,
    /// at bring-up, since `/System` cannot change under a running session.
    fn list_sound_themes() -> Vec<alloc::string::String> {
        let entries = list_store_dir(tairix_soundtheme::SOUND_STORE, "sound themes");
        tairix_soundtheme::themes(
            entries
                .iter()
                .flatten()
                .filter(|entry| entry.is_directory_backed())
                .map(tairix_browse::Entry::name),
        )
    }

    /// Spawn a desktop app as [`APP_ATTACH`] places it, forwarding the
    /// **user's environment** to it (`HOME`, `LANG`, …). Plain
    /// [`tairix_rt::spawn`] hands a child an *empty* environment; the desktop is
    /// the logged-in user's session, so an app it launches must inherit the
    /// same environment login exported and the session itself runs under —
    /// exactly as a login shell's children do. The file manager reads `HOME` to
    /// locate the user's Trash (`plans/NEW-FILEMANAGER.md` FM10), and apps read
    /// `LANG` for help localisation; forwarding the whole environment keeps the
    /// session from having to know which variables an app cares about. The
    /// environment is data and carries no authority.
    ///
    /// `args` are the app's arguments alone; the program itself is named by
    /// [`launch_argv`], which every launch goes through so no argument is
    /// read as the program's own name and lost. `stdin` is a descriptor of
    /// the session's to hand the app as its standard input — a document the
    /// app holds no authority to open itself.
    fn spawn_app(path: &[u8], args: &[&[u8]], stdin: Option<u32>) -> i64 {
        let count = tairix_rt::env_count();
        let mut env: alloc::vec::Vec<&[u8]> = alloc::vec::Vec::with_capacity(count as usize);
        for index in 0..count {
            if let Some(entry) = tairix_rt::env(index) {
                env.push(entry);
            }
        }
        let mut attach = APP_ATTACH;
        if let Some(fd) = stdin {
            attach.wires[STDIN as usize] = FdWire::Handle(fd);
        }
        tairix_rt::spawn_attached(path, &attach, &launch_argv(path, args), &env)
    }

    /// Everything a launch is *decided* with, bundled so it threads to each
    /// launch site without four more parameters each: the table the running
    /// instances are found in, and the manifest facts that say whether the
    /// bundle runs one of itself.
    ///
    /// The routes a live instance is reached *by* are not here: they need the
    /// shell and the compositor, which every launch site already holds, so
    /// they are bound at the call ([`LaunchCtx::launch`]) rather than
    /// borrowed for the life of this.
    struct LaunchCtx<'a> {
        launched: &'a mut LaunchTable,
        apps: &'a dyn AppBarBridge,
        server: &'a mut WindowServer<RtShmMapper>,
        sink: &'a mut RtEventSink,
        windows: &'a SessionWindows,
        identity: &'a RtWindowIdentity,
    }

    /// The routes half of a [`LaunchCtx`]: everything a launch needs *apart*
    /// from the table, so a function that already holds the table takes one
    /// parameter rather than four to reach a live instance.
    struct LaunchReach<'a> {
        apps: &'a dyn AppBarBridge,
        server: &'a mut WindowServer<RtShmMapper>,
        sink: &'a mut RtEventSink,
        windows: &'a SessionWindows,
        identity: &'a RtWindowIdentity,
    }

    /// The three routes a launch may reach a live instance by, over the
    /// session's real window channel.
    ///
    /// Bound for one launch: it holds the shell and compositor the raise
    /// needs, which are the caller's for the rest of the round.
    struct Reach<'a, 'b> {
        ctx: &'a mut LaunchCtx<'b>,
        shell: &'a mut DesktopShell,
        compositor: &'a mut Compositor,
    }

    impl Reach<'_, '_> {
        /// The instance `app`'s most recent window.
        fn recent_window(&self, app: ProcId) -> Option<tairix_wm::WindowId> {
            window_of_app(app, self.ctx.server, self.ctx.windows)
        }

        /// The one raise the user's own launch lends the instance it reaches.
        fn activation(&self) -> Activation {
            Activation::Granted {
                focus: self.ctx.windows.focused(self.shell),
            }
        }
    }

    impl LaunchHost for Reach<'_, '_> {
        fn queue_open_target(&mut self, app: ProcId, target: LaunchTarget<'_>) -> bool {
            let activation = self.activation();
            match self
                .ctx
                .server
                .hand_over_open_target(self.ctx.sink, app, activation, || {
                    open_entry(target, &mut RtDocumentRelay, app)
                }) {
                Ok(()) => true,
                Err(err) => {
                    // Stated, and answered honestly: the engine strands
                    // nothing, so the launch falls back to a fresh process.
                    app::report(
                        APP_NAME,
                        format_args!("cannot hand over an open target ({err:?})"),
                    );
                    false
                }
            }
        }

        fn ask_default(&mut self, app: ProcId) -> bool {
            let activation = self.activation();
            self.ctx
                .server
                .deliver_app_event(self.ctx.sink, app, &WindowEvent::AppBarDefault, activation)
                .is_ok()
        }

        fn raise_recent_window(&mut self, app: ProcId) -> bool {
            let Some(wm) = self.recent_window(app) else {
                return false;
            };
            self.shell.raise_window(self.compositor, wm)
        }
    }

    /// The session's live document relay: mint the instance that will show a
    /// document a one-shot delegation of it.
    ///
    /// A relayed grant is redeemed and handed on: the kernel copies the
    /// *first* grantor's captured identity onto the onward delegation rather
    /// than re-capturing it here, so the document is read under the authority
    /// of whoever opened it and never under the session's own, larger reach. A
    /// descriptor the session holds is one it opened for the user's own
    /// gesture on the desktop, so it is granted as it stands. A read-only
    /// delegation has no extent, so its ceiling is zero; a writable one passes
    /// on what its opener held, which the kernel can keep or shrink but never
    /// widen.
    struct RtDocumentRelay;

    /// The clipboard's reach into a region a client granted: mapped for the
    /// one copy and unmapped as the copy ends.
    struct RtPayloadRegions;

    impl tairix_desktop_session::clipboard::PayloadRegion for RtPayloadRegions {
        fn read(
            &mut self,
            region: ClientRegion,
            len: usize,
            into: &mut Vec<u8>,
        ) -> Result<(), Errno> {
            let mapped = tairix_rt::shm::MappedGrant::map(region.grantor, region.handle, len)?;
            let bytes = mapped.bytes().get(..len).ok_or(Errno::LengthOutOfRange)?;
            into.extend_from_slice(bytes);
            Ok(())
        }

        fn write(&mut self, region: ClientRegion, from: &[u8]) -> Result<bool, Errno> {
            let mut mapped = tairix_rt::shm::MappedGrant::map(region.grantor, region.handle, 0)?;
            let Some(to) = mapped.bytes_mut().get_mut(..from.len()) else {
                return Ok(false);
            };
            to.copy_from_slice(from);
            Ok(true)
        }
    }

    impl DocumentRelay for RtDocumentRelay {
        fn redeem(&mut self, grant: u64, from: ProcId) -> Result<u32, Errno> {
            let fd = Errno::syscall_result(tairix_rt::fd_redeem_from(grant, from))?;
            u32::try_from(fd).map_err(|_| Errno::OutOfRange)
        }

        fn mint(&mut self, fd: u32, writable: bool, app: ProcId) -> Result<u64, Errno> {
            let ceiling = tairix_browse::document::grant_ceiling(writable);
            let handle = Errno::syscall_result(tairix_rt::fd_grant(
                fd,
                ceiling,
                app,
                tairix_abi::GRANT_FILE_ONLY,
            ))?;
            // Handle zero is the kernel's reserved invalid value, never a mint.
            if handle == 0 {
                return Err(Errno::OutOfRange);
            }
            Ok(handle)
        }

        fn close(&mut self, fd: u32) {
            drop(tairix_rt::File::adopt(fd));
        }

        fn decline(&mut self, grant: u64, from: ProcId) {
            drop(tairix_rt::File::from_delegation_by(grant, from));
        }

        fn locate(&mut self, location: u64, from: ProcId) -> Result<alloc::string::String, Errno> {
            let mut path = alloc::vec![0u8; tairix_abi::FS_PATH_MAX];
            match tairix_rt::fd_redeem_location(location, from, &mut path) {
                Ok(len) => {
                    path.truncate(len);
                    alloc::string::String::from_utf8(path).map_err(|_| Errno::OutOfRange)
                }
                // A file delegation named in a location's place stays pending
                // until it is taken.
                Err(Errno::OutOfRange) => {
                    self.decline(location, from);
                    Err(Errno::OutOfRange)
                }
                Err(err) => Err(err),
            }
        }
    }

    impl LaunchCtx<'_> {
        /// The desktop's one launch funnel: resolve a launch of the bundle
        /// whose entry binary is `run_path` and carry it out.
        ///
        /// `args` are the arguments a *fresh* process is given; `target` is
        /// the document or folder the launch named, which a *running*
        /// instance is handed instead. Answers the pid a spawn was admitted
        /// as, or the live instance a relaunch reached.
        ///
        /// Singleton is the default, so relaunching a bundle that is already
        /// running asks the running instance to open rather than starting a
        /// second process. A bundle with no live instance is spawned without
        /// its manifest even being read: there is nothing to reach, so the
        /// answer cannot depend on what the manifest says.
        fn launch(
            &mut self,
            shell: &mut DesktopShell,
            compositor: &mut Compositor,
            run_path: &str,
            label: &str,
            args: &[&[u8]],
            target: Option<LaunchTarget<'_>>,
        ) -> Option<u64> {
            let launched = match self.reach_running(shell, compositor, run_path, target) {
                Some(pid) => Some(pid),
                None => spawn_and_record(self.launched, run_path, label, args),
            };
            if launched.is_some() {
                self.launched.note_used(run_path);
            }
            launched
        }

        /// Hand the document `opened` for the bundle whose entry binary is
        /// `run_path` over: to a live instance that takes it, else to a fresh
        /// process as its standard input.
        ///
        /// The session opened it, under its own authority, because the user
        /// opened it from the desktop the session shows, and an application
        /// that requests no filesystem capability can do nothing with a path.
        fn launch_document(
            &mut self,
            shell: &mut DesktopShell,
            compositor: &mut Compositor,
            run_path: &str,
            label: &str,
            name: &str,
            opened: &tairix_browse::document::Opened,
        ) -> Option<u64> {
            let fd = opened.file.fd();
            // A name the channel cannot carry is left unknown rather than
            // refusing the document it names.
            let Ok(title) = DocumentName::new(name).or_else(|_| DocumentName::new("")) else {
                return None;
            };
            let target = LaunchTarget::Document {
                name: &title,
                authority: DocumentAuthority::Held { fd },
                writable: opened.writable,
            };
            let launched = match self.reach_running(shell, compositor, run_path, Some(target)) {
                Some(pid) => Some(pid),
                None => record_launch(
                    self.launched,
                    spawn_app(
                        run_path.as_bytes(),
                        &[
                            tairix_browse::document::role_arg(opened.writable),
                            title.as_str().as_bytes(),
                        ],
                        Some(fd),
                    ),
                    label,
                    run_path,
                ),
            };
            if launched.is_some() {
                self.launched.note_used(run_path);
            }
            launched
        }

        /// Hand the file `opened` read-only to the window `placed` names, to be
        /// placed in its document: queued for the application owning it, or
        /// answering why it could not be.
        fn place_document(
            &mut self,
            placed: &PlacedItem,
            opened: &tairix_browse::document::Opened,
        ) -> Result<(), PlaceRefused> {
            let PlacedItem {
                placement, name, ..
            } = placed;
            let app = self
                .server
                .owner_of(placement.window_id)
                .ok_or(PlaceRefused::WindowClosed)?;
            let fd = opened.file.fd();
            let handed =
                self.server
                    .hand_over_open_target(self.sink, app, Activation::Withheld, || {
                        let grant =
                            RtDocumentRelay.relay(DocumentAuthority::Held { fd }, false, app)?;
                        Ok(OpenEntry::Placed {
                            placement: *placement,
                            name: *name,
                            grant,
                        })
                    });
            handed.map_err(PlaceRefused::Untaken)
        }

        /// Hand the desktop's file manager `target`, starting it again with
        /// `args` only where it has gone. It is the desktop's own and never
        /// quits, so a second is never started beside one still starting or
        /// one that turned the target away: that is stated instead.
        fn hand_file_manager(
            &mut self,
            (shell, compositor): (&mut DesktopShell, &mut Compositor),
            args: &[&[u8]],
            target: LaunchTarget<'_>,
        ) {
            let found = match self.launched.running_from(FILES_RUN_PATH) {
                None => FileManagerFound::Gone,
                Some(pid) => self
                    .identity
                    .proc_id_of(pid)
                    .map_or(FileManagerFound::Starting, FileManagerFound::Live),
            };
            let mut reach = Reach {
                ctx: self,
                shell,
                compositor,
            };
            match hand_file_manager(&mut reach, found, target) {
                FileManagerHand::Start => {
                    let _ = spawn_and_record(self.launched, FILES_RUN_PATH, FILES_LABEL, args);
                }
                FileManagerHand::Handed => {}
                FileManagerHand::Wait => {
                    let held = HeldTarget::of(target)
                        .is_some_and(|held| self.launched.hold_for_file_manager(held));
                    if !held {
                        app::report(
                            APP_NAME,
                            format_args!(
                                "the file manager is starting and holds as much as it can take \
                                 meanwhile; open it again once it shows"
                            ),
                        );
                    }
                }
                FileManagerHand::Refused => app::report(
                    APP_NAME,
                    format_args!("the file manager could not take what was opened"),
                ),
            }
        }

        /// Hand the file manager what was opened while it was starting, once
        /// it can take it; held meanwhile, and through a restart.
        fn deliver_held_for_file_manager(
            &mut self,
            (shell, compositor): (&mut DesktopShell, &mut Compositor),
        ) {
            let Some(app) = self
                .launched
                .running_from(FILES_RUN_PATH)
                .and_then(|pid| self.identity.proc_id_of(pid))
            else {
                return;
            };
            for held in self.launched.take_held_for_file_manager() {
                let mut reach = Reach {
                    ctx: self,
                    shell,
                    compositor,
                };
                if hand_file_manager(&mut reach, FileManagerFound::Live(app), held.target())
                    != FileManagerHand::Handed
                {
                    app::report(
                        APP_NAME,
                        format_args!("the file manager could not take what was opened"),
                    );
                }
            }
        }

        /// Offer a launch of `run_path` naming `target` to its live instance,
        /// answering the recorded child that took it — or `None`, when the
        /// launch must spawn.
        fn reach_running(
            &mut self,
            shell: &mut DesktopShell,
            compositor: &mut Compositor,
            run_path: &str,
            target: Option<LaunchTarget<'_>>,
        ) -> Option<u64> {
            let running = self.launched.running_from(run_path)?;
            let app = self.identity.proc_id_of(running)?;
            let bundle = tairix_appstore::bundle_of_entry(run_path).unwrap_or(run_path);
            let one_instance = self.apps.runs_one_instance(bundle);
            let mut reach = Reach {
                ctx: self,
                shell,
                compositor,
            };
            match resolve_launch(&mut reach, Some(app), one_instance, target) {
                Launch::Spawn => None,
                Launch::Reused { .. } => Some(running),
            }
        }
    }

    /// Spawn `run_path` and record the launch: the half of a launch that
    /// starts a process, shared by the funnel and by the two bring-up
    /// launches that run before any instance exists.
    fn spawn_and_record(
        launched: &mut LaunchTable,
        run_path: &str,
        label: &str,
        args: &[&[u8]],
    ) -> Option<u64> {
        record_launch(
            launched,
            spawn_app(run_path.as_bytes(), args, None),
            label,
            run_path,
        )
    }

    /// Record a just-issued launch, answering with the pid recorded.
    ///
    /// Asynchronous launch admits the child and returns its PID before the
    /// image is loaded, so a successful admit only *starts* the launch:
    /// remember the PID under its display label (so the `CHILD_TOKEN` reap
    /// can name the app if its load is later refused via the child's
    /// reserved-`LOAD_*` exit status) and its spawn path (its attested
    /// bundle identity). Answering with that pid is what lets a caller act
    /// on the child it just started instead of searching the table for it.
    ///
    /// A result that is not a task id — a stripped spawn capability, a
    /// malformed path, any refusal decided before a child exists — records
    /// nothing, is reported fail-loud at once, and answers `None`. A
    /// denied optional launch never ends the session.
    fn record_launch(
        launched: &mut LaunchTable,
        ret: i64,
        label: &str,
        run_path: &str,
    ) -> Option<u64> {
        let Some(pid) = admitted_pid(ret) else {
            app::report(APP_NAME, format_args!("{label} launch refused"));
            return None;
        };
        launched.record(pid, label, run_path, tairix_rt::clock_get());
        Some(pid)
    }

    /// Carry out what a pick asked for: the chosen file — or the chosen
    /// folder's files — opened on the reader of its volume for the window's
    /// owner as `openers` says it opens documents, and a pick the user walked
    /// away from concluded.
    #[allow(clippy::too_many_arguments)] // The serve loop's whole mutable state, threaded explicitly.
    fn step_pick<S: DirectorySource, F: PickSources<S>>(
        step: Option<PickStep>,
        openers: &Openers<'_>,
        files: &Listings,
        server: &mut WindowServer<RtShmMapper>,
        sink: &mut RtEventSink,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        windows: &mut SessionWindows,
        picker: &mut SessionPicker<S, F>,
        apps: &mut AppBarPanel,
        menu: &mut MenuChain,
    ) {
        let answered = match step {
            None => return,
            Some(PickStep::Cancelled { pick, for_window }) => {
                let asked = (pick, for_window);
                conclude(
                    asked, None, server, sink, shell, compositor, windows, picker, apps, menu,
                );
                return;
            }
            Some(PickStep::Menu {
                pick,
                model,
                anchor,
                for_window,
            }) => {
                let geom = chain_geometry(shell.session(), compositor);
                match open_desktop_menu(
                    menu,
                    ChainOwner::Picker { pick, for_window },
                    model,
                    window_menu_placement(anchor),
                    false,
                    &geom,
                ) {
                    Ok(()) => present_menu_chain(menu, shell, compositor, windows),
                    Err(refused) => {
                        app::report(APP_NAME, format_args!("no save menu ({refused:?})"));
                    }
                }
                return;
            }
            Some(PickStep::NewFolder { pick, serial, path }) => {
                files.submit(FileJob::NewFolder { pick, serial, path })
            }
            Some(PickStep::Open {
                pick,
                serial,
                for_window,
                path,
                access,
                typing,
            }) => {
                let owner = server.owner_of(for_window);
                files.submit(open_job(
                    (pick, serial),
                    path,
                    (access, typing),
                    owner,
                    openers,
                ))
            }
        };
        // Answered at once: carried out here for want of a reader, or
        // refused because too many calls are waiting.
        match answered {
            Some(FileAnswer::Pick {
                pick,
                serial,
                opened,
                created,
                made,
            }) => {
                let discard = settle_pick(
                    (pick, serial),
                    (opened, created, made),
                    server,
                    sink,
                    shell,
                    compositor,
                    windows,
                    picker,
                    apps,
                    menu,
                );
                if let Some(job) = discard {
                    let _ = files.submit(job);
                }
            }
            Some(FileAnswer::Folder {
                pick,
                serial,
                opened,
            }) => {
                let asked = (pick, serial);
                settle_folder_pick(
                    asked, opened, server, sink, shell, compositor, windows, picker, apps, menu,
                );
            }
            Some(FileAnswer::NewFolder {
                pick, serial, made, ..
            }) => {
                picker.folder_made(pick, serial, made, shell, compositor);
            }
            Some(FileAnswer::Desktop(_) | FileAnswer::Discarded) | None => {}
        }
    }

    /// What a reader carries out for pick `pick`'s attempt `serial` to open
    /// `path` for the window's attested `owner`: a folder pick opens the
    /// folder's files of the kinds the owner opens, an import the file
    /// read-only, any other the file, for writing where the owner edits
    /// documents.
    fn open_job(
        (pick, serial): (PickId, u64),
        path: alloc::string::String,
        (access, typing): (PickAccess, Typing),
        owner: Option<tairix_abi::ProcId>,
        openers: &Openers<'_>,
    ) -> FileJob {
        if access == PickAccess::Folder {
            let kinds = owner.map(|owner| openers.kinds(owner)).unwrap_or_default();
            return FileJob::Folder {
                pick,
                serial,
                path,
                kinds,
            };
        }
        FileJob::Pick {
            pick,
            serial,
            path,
            access,
            edits: access.opens_to_edit(owner.is_some_and(|owner| openers.edits(owner))),
            typing,
        }
    }

    /// Settle the folder a reader opened for pick `pick`'s attempt `serial`:
    /// delegate each file one-shot to the attested owner of the window that
    /// asked and deliver `FolderPicked`, counting a file whose grant is refused
    /// among those left out. A folder that could not be read leaves the picker
    /// up to say why; an answer the pick no longer waits for is dropped, and
    /// the files it opened close with it.
    #[allow(clippy::too_many_arguments)] // The serve loop's whole mutable state, threaded explicitly.
    fn settle_folder_pick<S: DirectorySource, F: PickSources<S>>(
        (pick, serial): (PickId, u64),
        result: Result<FolderFiles, Errno>,
        server: &mut WindowServer<RtShmMapper>,
        sink: &mut RtEventSink,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        windows: &mut SessionWindows,
        picker: &mut SessionPicker<S, F>,
        apps: &mut AppBarPanel,
        menu: &mut MenuChain,
    ) {
        let status = result.as_ref().map(|_| ()).map_err(|err| *err);
        let Some(PickEnd::Chosen { for_window, .. }) =
            picker.opened(pick, serial, (status, None), shell, compositor)
        else {
            return;
        };
        let Ok(found) = result else {
            return;
        };
        let mut files = alloc::collections::VecDeque::new();
        let mut left_out = found.left_out;
        for (file, name) in found.files {
            let granted = DocumentName::new(&name)
                .ok()
                .zip(delegate(&file, for_window, server));
            match granted {
                Some((name, handle)) => {
                    files.push_back(tairix_abi::window_ipc::PickedFile { handle, name });
                }
                None => left_out = left_out.saturating_add(1),
            }
        }
        release_pick_hold(pick, windows, shell, compositor);
        let Some(owner) = server.owner_of(for_window) else {
            return;
        };
        if let Err(Errno::NotFound) = server.conclude_folder_pick(sink, for_window, files, left_out)
        {
            drop_departed(
                owner,
                server,
                shell,
                compositor,
                windows,
                picker,
                &mut apps.service,
                menu,
            );
        }
    }

    /// Settle the open a reader carried out for pick `pick`'s attempt
    /// `serial`: delegate the file one-shot to the attested owner of the
    /// window that asked and deliver `FilePicked`, or — when the open or the
    /// grant was refused — state why and deliver `PickCancelled`, since
    /// nothing was delegated. An answer the pick no longer waits for — it
    /// closed, or moved on — is dropped, and the file it opened closes with
    /// it; one the open `created` is answered with the job that takes it back.
    #[allow(clippy::too_many_arguments)] // The serve loop's whole mutable state, threaded explicitly.
    fn settle_pick<S: DirectorySource, F: PickSources<S>>(
        (pick, serial): (PickId, u64),
        (result, created, made): (Opened, Option<alloc::string::String>, alloc::string::String),
        server: &mut WindowServer<RtShmMapper>,
        sink: &mut RtEventSink,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        windows: &mut SessionWindows,
        picker: &mut SessionPicker<S, F>,
        apps: &mut AppBarPanel,
        menu: &mut MenuChain,
    ) -> Option<FileJob> {
        let opened = match &result {
            Ok(_) => Ok(()),
            Err(err) => Err(*err),
        };
        let answer = (opened, Some(made));
        let (for_window, chosen) = match picker.opened(pick, serial, answer, shell, compositor) {
            None => {
                return created
                    .zip(result.ok())
                    .map(|(path, made)| FileJob::Discard {
                        file: made.file,
                        path,
                    });
            }
            Some(PickEnd::Refused { for_window, .. }) => {
                if let Err(err) = result {
                    app::report(
                        APP_NAME,
                        format_args!("the chosen file could not be opened ({err})"),
                    );
                }
                (for_window, None)
            }
            Some(PickEnd::Chosen {
                for_window, name, ..
            }) => {
                let mut kept = None;
                let granted = result.ok().and_then(|file| {
                    let granted = DocumentName::new(&name)
                        .ok()
                        .zip(delegate(&file, for_window, server))
                        .map(|(name, handle)| (handle, name, file.writable));
                    if granted.is_none() {
                        kept = Some(file);
                    }
                    granted
                });
                if granted.is_none() {
                    io::write_stderr_line("desktop: picker delegation refused");
                }
                // A file made for a requester that cannot be handed it is not
                // left behind, empty, under the name the user chose.
                if let Some((path, made)) = created.zip(kept) {
                    let asked = (pick, for_window);
                    conclude(
                        asked, None, server, sink, shell, compositor, windows, picker, apps, menu,
                    );
                    return Some(FileJob::Discard {
                        file: made.file,
                        path,
                    });
                }
                (for_window, granted)
            }
        };
        conclude(
            (pick, for_window),
            chosen.as_ref().map(|(handle, name, writable)| PickedFile {
                handle: *handle,
                name,
                writable: *writable,
            }),
            server,
            sink,
            shell,
            compositor,
            windows,
            picker,
            apps,
            menu,
        );
        None
    }

    /// Deliver pick `pick`'s conclusion to window `window_id` — the delegation
    /// handle and the chosen name, or nothing chosen — tearing its owner's
    /// windows down when the owner's event port is gone.
    #[allow(clippy::too_many_arguments)] // The serve loop's whole mutable state, threaded explicitly.
    fn conclude<S: DirectorySource, F: PickSources<S>>(
        (pick, window_id): (PickId, u64),
        chosen: Option<PickedFile<'_>>,
        server: &mut WindowServer<RtShmMapper>,
        sink: &mut RtEventSink,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        windows: &mut SessionWindows,
        picker: &mut SessionPicker<S, F>,
        apps: &mut AppBarPanel,
        menu: &mut MenuChain,
    ) {
        release_pick_hold(pick, windows, shell, compositor);
        let Some(owner) = server.owner_of(window_id) else {
            return;
        };
        if let Err(Errno::NotFound) = server.conclude_pick(sink, window_id, chosen) {
            drop_departed(
                owner,
                server,
                shell,
                compositor,
                windows,
                picker,
                &mut apps.service,
                menu,
            );
        }
    }

    /// Pick `pick` concluded: the window it held takes input again, and the
    /// keyboard the picker handed back goes to whatever still holds it.
    fn release_pick_hold(
        pick: PickId,
        windows: &mut SessionWindows,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
    ) {
        if let Some(front) = windows.release_pick_hold(pick, shell.router().focused()) {
            let _ = shell.focus_holder(compositor, front);
        }
    }

    /// Tell the window that asked for a system task how it ended, tearing its
    /// owner's windows down when the owner's event port is gone.
    #[allow(clippy::too_many_arguments)] // The serve loop's whole mutable state, threaded explicitly.
    fn settle_authorisation<S: DirectorySource, F: PickSources<S>>(
        (asker, outcome, output): (AuthoriseFor, AuthorisedOutcome, Vec<u8>),
        server: &mut WindowServer<RtShmMapper>,
        sink: &mut RtEventSink,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        windows: &mut SessionWindows,
        picker: &mut SessionPicker<S, F>,
        apps: &mut AppBarPanel,
        menu: &mut MenuChain,
    ) {
        let owner = match asker {
            AuthoriseFor::Window(window_id) => server.owner_of(window_id),
            AuthoriseFor::Slot(app) => Some(app),
        };
        let Some(owner) = owner else {
            return;
        };
        match server.conclude_authorisation(sink, asker, outcome, output) {
            Ok(()) => {}
            Err(Errno::NotFound) => drop_departed(
                owner,
                server,
                shell,
                compositor,
                windows,
                picker,
                &mut apps.service,
                menu,
            ),
            // The answer could never be delivered, so it is stated, not lost
            // unseen.
            Err(refused) => io::write_stderr_line(format_args!(
                "desktop: an application could not be told how its authorisation ended: {refused}"
            )),
        }
    }

    /// Tell the asking window its preview concluded: drawn into its region,
    /// or refused, so the tile draws its placeholder rather than waiting for a
    /// picture that is not coming. A window that has closed under one is
    /// simply not there to deliver to.
    #[allow(clippy::too_many_arguments)] // The serve loop's whole mutable state, threaded explicitly.
    fn settle_wallpaper_preview<S: DirectorySource, F: PickSources<S>>(
        done: PreviewDone,
        server: &mut WindowServer<RtShmMapper>,
        sink: &mut RtEventSink,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        windows: &mut SessionWindows,
        picker: &mut SessionPicker<S, F>,
        apps: &mut AppBarPanel,
        menu: &mut MenuChain,
    ) {
        let event = WindowEvent::PreviewRendered {
            window_id: done.request.window_id,
            subject: done.request.size.subject,
            width: done.request.size.width,
            height: done.request.size.height,
            outcome: done.outcome,
        };
        deliver(
            server,
            sink,
            shell,
            compositor,
            windows,
            picker,
            &mut apps.service,
            menu,
            &event,
        );
    }

    /// Mint the attested owner of window `window_id` a one-shot delegation of
    /// the file a pick `opened`, answering the `fd_redeem` handle. Every
    /// refusal answers `None` (fail closed, nothing delegated).
    ///
    /// A file opened to read is delegated read-only, with no extent to bound;
    /// one opened to save is delegated write-only with the session's own
    /// reach, which the user's quota and the volume bound.
    ///
    /// The owner the compositor records *is* the attested instance, so the
    /// grant names it directly. A pick concludes an arbitrary time after the
    /// app asked for it, so a task id learned back then could name a later
    /// holder by now; an instance names one process for all time, and a
    /// window whose app has since exited simply resolves to nothing.
    fn delegate(
        opened: &tairix_browse::document::Opened,
        window_id: u64,
        server: &WindowServer<RtShmMapper>,
    ) -> Option<u64> {
        let owner = server.owner_of(window_id)?;
        let ceiling = tairix_browse::document::grant_ceiling(opened.writable);
        let handle = tairix_rt::fd_grant(opened.file.fd(), ceiling, owner, 0);
        u64::try_from(handle).ok().filter(|&handle| handle != 0)
    }

    /// Move what the apps were told of the keyboard, `told`, to `now`: the
    /// window that lost it learns first, then the one that gained it.
    #[allow(clippy::too_many_arguments)] // `deliver`'s context, and the two places.
    fn tell_focus<S: DirectorySource, F: PickSources<S>>(
        told: &mut Option<u64>,
        now: Option<u64>,
        server: &mut WindowServer<RtShmMapper>,
        sink: &mut RtEventSink,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        windows: &mut SessionWindows,
        picker: &mut SessionPicker<S, F>,
        apps: &mut dyn AppBarBridge,
        menu: &mut MenuChain,
    ) {
        for event in focus_reports(*told, now) {
            deliver(
                server, sink, shell, compositor, windows, picker, apps, menu, &event,
            );
        }
        *told = now;
    }

    /// Tell the apps where the keyboard is after it moved without the press
    /// that reports it — a window raised from its slot, a popup opened over
    /// its owner, a window closed — so one that lost it learns so before its
    /// next input.
    #[allow(clippy::too_many_arguments)] // `deliver`'s context, and what the apps were told.
    fn reconcile_focus<S: DirectorySource, F: PickSources<S>>(
        told: &mut Option<u64>,
        server: &mut WindowServer<RtShmMapper>,
        sink: &mut RtEventSink,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        windows: &mut SessionWindows,
        picker: &mut SessionPicker<S, F>,
        apps: &mut dyn AppBarBridge,
        menu: &mut MenuChain,
    ) {
        let now = windows.focused(shell);
        tell_focus(
            told, now, server, sink, shell, compositor, windows, picker, apps, menu,
        );
    }

    /// Tell the clients the seat took the pointer from what they are owed:
    /// the one whose press was taken, that the press is over, the one whose
    /// window the pointer left, that nothing in it is under the pointer, and
    /// one whose window a resize was dragging, the size it settled at.
    #[allow(clippy::too_many_arguments)] // The serve loop's whole mutable state, threaded explicitly.
    fn deliver_abandoned<S: DirectorySource, F: PickSources<S>>(
        server: &mut WindowServer<RtShmMapper>,
        sink: &mut RtEventSink,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        windows: &mut SessionWindows,
        picker: &mut SessionPicker<S, F>,
        apps: &mut dyn AppBarBridge,
        menu: &mut MenuChain,
    ) {
        let abandoned = shell.take_abandoned();
        for gone in [abandoned.client, abandoned.left].into_iter().flatten() {
            tell_pointer_gone(
                gone, server, sink, shell, compositor, windows, picker, apps, menu,
            );
        }
        let settled = abandoned
            .resized
            .and_then(|wm| resize_drag_event(wm, true, shell, compositor, windows));
        let left = abandoned
            .moved
            .and_then(|wm| tool_move_abandoned_event(wm, windows));
        for event in [settled, left].into_iter().flatten() {
            deliver(
                server, sink, shell, compositor, windows, picker, apps, menu, &event,
            );
        }
    }

    /// Tell the owner of the window `wm` that the pointer is no longer its
    /// own, at `at` in its client: the trusted picker or an alert directly,
    /// and an application's window over its channel.
    #[allow(clippy::too_many_arguments)] // The serve loop's whole mutable state, threaded explicitly.
    fn tell_pointer_gone<S: DirectorySource, F: PickSources<S>>(
        (wm, at): (tairix_wm::WindowId, tairix_wm::Point),
        server: &mut WindowServer<RtShmMapper>,
        sink: &mut RtEventSink,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        windows: &mut SessionWindows,
        picker: &mut SessionPicker<S, F>,
        apps: &mut dyn AppBarBridge,
        menu: &mut MenuChain,
    ) {
        if let Some(pick) = picker.shown_in(wm) {
            picker.handle_pointer(
                pick,
                at,
                &tairix_wm::InputEvent::PointerCancelled,
                shell,
                compositor,
            );
        } else if windows.alerts.owns(wm) {
            // A cancel completes no click, so it answers nothing.
            let _ = windows.alerts.pointer(
                wm,
                at,
                &tairix_wm::InputEvent::PointerCancelled,
                tairix_rt::clock_get(),
                &mut windows.holds,
                shell,
                compositor,
            );
        } else if let (Some(window_id), Ok(x), Ok(y)) =
            (windows.ipc_id(wm), u32::try_from(at.x), u32::try_from(at.y))
        {
            let event = WindowEvent::Pointer {
                window_id,
                x,
                y,
                action: PointerAction::Cancelled,
                modifiers: pointer_modifiers(shell),
                at_ns: tairix_rt::clock_get(),
            };
            deliver(
                server, sink, shell, compositor, windows, picker, apps, menu, &event,
            );
        }
    }

    /// Deliver a redraw request to the owning app of every window whose
    /// content the compositor gave back (or found missing when the window
    /// was shown again).
    ///
    /// The window manager queues window-manager ids and knows nothing of
    /// the window protocol; the session maps each to its client window
    /// through the one table it already keeps and sends the protocol's
    /// redraw event. A window with no served client — the taskbar, a
    /// session-owned popup — has nothing to ask and is skipped: the
    /// session paints those itself.
    #[allow(clippy::too_many_arguments)] // The serve loop's whole mutable state, threaded explicitly.
    fn deliver_pending_redraws<S: DirectorySource, F: PickSources<S>>(
        server: &mut WindowServer<RtShmMapper>,
        sink: &mut RtEventSink,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        windows: &mut SessionWindows,
        picker: &mut SessionPicker<S, F>,
        apps: &mut dyn AppBarBridge,
        menu: &mut MenuChain,
    ) {
        for wm in compositor.pending_redraws() {
            let Some(window_id) = windows.ipc_id(wm) else {
                continue;
            };
            deliver(
                server,
                sink,
                shell,
                compositor,
                windows,
                picker,
                apps,
                menu,
                &WindowEvent::RedrawRequested { window_id },
            );
        }
    }

    /// Deliver the app-ward events the host produced while answering a
    /// request and could not send itself, because the engine held the
    /// borrow the delivery needs — the same reason the identity pass and
    /// the menu chain are answered out here.
    #[allow(clippy::too_many_arguments)] // The serve loop's whole mutable state, threaded explicitly.
    fn deliver_owed_events<S: DirectorySource, F: PickSources<S>>(
        server: &mut WindowServer<RtShmMapper>,
        sink: &mut RtEventSink,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        windows: &mut SessionWindows,
        picker: &mut SessionPicker<S, F>,
        apps: &mut dyn AppBarBridge,
        menu: &mut MenuChain,
    ) {
        for event in windows.take_owed_events() {
            deliver(
                server, sink, shell, compositor, windows, picker, apps, menu, &event,
            );
        }
    }

    /// Tell every client whose window content was released while nobody could
    /// see it that the session has let go of its frames, unmapping this side
    /// first.
    ///
    /// Both sides have to let go for the pages to be freed: the compositor's
    /// copy of a window's pixels is one of three, and the other two — the
    /// app's render target and the frame region it presents from — are the
    /// client's. Unmapping here before the event goes out means any present
    /// that crosses it is refused typed rather than writing into a mapping
    /// that is about to vanish; the client library re-attaches on the paint
    /// that follows the redraw request the window's next showing sends.
    #[allow(clippy::too_many_arguments)] // The delivery path's whole mutable state, threaded explicitly.
    fn deliver_released_notices<S: DirectorySource, F: PickSources<S>>(
        server: &mut WindowServer<RtShmMapper>,
        sink: &mut RtEventSink,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        windows: &mut SessionWindows,
        picker: &mut SessionPicker<S, F>,
        apps: &mut dyn AppBarBridge,
        menu: &mut MenuChain,
    ) {
        for wm in compositor.take_released_notices() {
            let Some(window_id) = windows.ipc_id(wm) else {
                continue;
            };
            let bytes = server.release_frames(window_id);
            // Its pixels are gone, so the window is awaiting them again and
            // the shown announcement must be re-earned by the frame that
            // brings them back.
            windows.content_released(window_id);
            log_info(
                CONTENT_RELEASED,
                CONTENT_RELEASED_MESSAGE,
                &[
                    LogField {
                        key: "window",
                        value: LogFieldValue::UnsignedInt(window_id),
                    },
                    LogField {
                        key: "bytes",
                        value: LogFieldValue::UnsignedInt(bytes),
                    },
                ],
            );
            deliver(
                server,
                sink,
                shell,
                compositor,
                windows,
                picker,
                apps,
                menu,
                &WindowEvent::ContentReleased { window_id },
            );
        }
    }

    /// Publish the desktop every application shares, so each converges on
    /// the state the session is actually compositing.
    ///
    /// The screen extent, the UI scale, and the active appearance are
    /// properties of the seat, and an application holds its own pixels — so
    /// nothing the session does to its own surfaces can bring an app's window
    /// into step. One publish reaches every subscriber, windowed or not: an
    /// application closed to its icon-bar slot is told too, and opens its
    /// next window in the appearance in force rather than the one it last
    /// saw. Publishing the value already in force wakes nobody, so this is
    /// safe to call from any path that *might* have moved it.
    ///
    /// A desktop the record cannot describe, or a publish the kernel refuses
    /// (this session does not hold the seat's live display lease — it is in
    /// the background, and re-publishes when it re-acquires), is reported and
    /// nothing is published: an application keeps the last state it was given
    /// rather than being handed a guess.
    fn publish_desktop(compositor: &Compositor, shell: &DesktopShell) {
        let desktop = match desktop_info(compositor, shell) {
            Ok(desktop) => desktop,
            Err(err) => {
                app::report(
                    APP_NAME,
                    format_args!("cannot describe the desktop to apps: {err}"),
                );
                return;
            }
        };
        let rc = tairix_rt::notice_publish(&Notice::Desktop(desktop));
        if rc < 0 {
            app::report(
                APP_NAME,
                format_args!(
                    "could not publish the desktop to apps: {}",
                    Errno::from_syscall(rc)
                ),
            );
        }
    }

    /// The wire modifiers a pointer event delivered right now carries: the
    /// seat's held set, in the ABI vocabulary.
    ///
    /// A modifier key reaches no surface as a key, so an application cannot
    /// track the seat's state itself; stamping it here is what lets one
    /// qualify a click (a shift-click) by what is held.
    fn pointer_modifiers(shell: &DesktopShell) -> AbiModifiers {
        modifiers_to_abi(shell.modifiers())
    }

    /// Relay the user's `answer` to an application's alert. It is the
    /// user's gesture toward that application, so it lends a raise while the
    /// keyboard stays where the answer left it.
    fn relay_alert_answer(
        answer: tairix_desktop_session::alert::Answer,
        server: &mut WindowServer<RtShmMapper>,
        sink: &mut RtEventSink,
        shell: &DesktopShell,
        windows: &SessionWindows,
    ) {
        let activation = Activation::Granted {
            focus: windows.focused(shell),
        };
        // An application gone since cannot be answered; its alert is down.
        let _ = server.answer_alert(sink, answer.alert_id, answer.index, activation);
    }

    /// Deliver one app-ward event, tearing the owner's windows down when
    /// the kernel proves the owner is gone (its event port was reclaimed,
    /// so the send finds nothing), and answering whether it was delivered.
    #[allow(clippy::too_many_arguments)] // The serve loop's whole mutable state, threaded explicitly.
    fn deliver<S: DirectorySource, F: PickSources<S>>(
        server: &mut WindowServer<RtShmMapper>,
        sink: &mut RtEventSink,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        windows: &mut SessionWindows,
        picker: &mut SessionPicker<S, F>,
        apps: &mut dyn AppBarBridge,
        menu: &mut MenuChain,
        event: &WindowEvent,
    ) -> bool {
        // Window-scoped only: an icon-bar event names no window and goes
        // out through `relay_app_bar`, which addresses the declaration
        // instead.
        let Some(owner) = event.window_id().and_then(|id| server.owner_of(id)) else {
            return false;
        };
        // A held window takes no new input until its holder is gone, and a
        // dialog no press until it has been seen.
        if !windows.admits(event, tairix_rt::clock_get) {
            return false;
        }
        match server.deliver_event(sink, event) {
            Ok(()) => {
                windows.note_delivered(event);
                true
            }
            Err(Errno::NotFound) => {
                drop_departed(
                    owner, server, shell, compositor, windows, picker, apps, menu,
                );
                false
            }
            Err(_) => false,
        }
    }

    /// Tear down the windows of `owner`, whose event port a delivery found
    /// gone.
    ///
    /// The caller proved the window exists, so a `NotFound` is the sink's: the
    /// owner's event port is gone — the kernel reclaimed it at exit — and its
    /// windows go with it. A merely full mailbox never reaches here: the sink
    /// holds that event and answers for it.
    #[allow(clippy::too_many_arguments)] // The teardown's whole mutable surround, threaded explicitly.
    fn drop_departed<S: DirectorySource, F: PickSources<S>>(
        owner: ProcId,
        server: &mut WindowServer<RtShmMapper>,
        shell: &mut DesktopShell,
        compositor: &mut Compositor,
        windows: &mut SessionWindows,
        picker: &mut SessionPicker<S, F>,
        apps: &mut dyn AppBarBridge,
        menu: &mut MenuChain,
    ) {
        let mut bridge = ShellWindowHost {
            shell,
            compositor,
            windows,
            picker,
            apps,
            menu,
            // A teardown serves no `OpenMenu`, so this bridge cannot vouch
            // for the seat and says so rather than claiming it free.
            seat_held: true,
            screensaver: None,
            relay: &mut RtDocumentRelay,
            wallpapers: &mut NoGallery,
            cursor_sets: &[],
            clipboard: &mut tairix_desktop_session::clipboard::NoClipboard,
            authorise: None,
        };
        server.client_exited(&mut bridge, owner);
    }

    /// Program entry point. `tairix-rt`'s `_start` calls it once the
    /// runtime is set up and routes its return value through the `exit`
    /// syscall.
    ///
    /// Exit codes: `0` for a served short help, `1` on a usage error, and
    /// otherwise the session's own codes — on success this never returns
    /// until the session ends: the loop runs until the seat is lost or a
    /// fault ends it.
    fn main() -> i32 {
        // The sandbox worker role first, before any argument parsing or
        // seat work: when the session spawns its icon worker it re-enters
        // this same binary with the reserved role argument, and that
        // capability-empty child must serve parses and nothing else.
        if worker_role() {
            return serve_stdio(&mut ImageRenderService::default()).exit_code();
        }
        // The command surface next: the reserved short-help switches never
        // touch the seat.
        let Some(arguments) = tairix_help::arguments(APP_NAME) else {
            return tairix_help::MISUSE;
        };
        match parse(&arguments) {
            Ok(Command::Run) => {}
            Ok(Command::Help) => return tairix_help::print_own_short_help(APP_NAME, Some(USAGE)),
            Err(err @ CliError::Usage) => {
                tairix_help::report_misuse(APP_NAME, &err);
                return tairix_help::MISUSE;
            }
        }

        // From here this task is the desktop's compositor loop, so declare
        // the frame it owes the user. A debug image then reports any span
        // that overruns, naming the call that spent it; a shippable one
        // arms nothing and answers zero, which is why the result is not
        // examined.
        let _ = tairix_rt::latency_watch(DEFAULT_FRAME_BUDGET_NS);

        // Acquire the boot seat's exclusive, revocable lease. The kernel
        // binds this task as the owner; a seat already held refuses with a
        // typed error rather than displacing its owner.
        if tairix_rt::display_acquire(SEAT_PRIMARY) < 1 {
            return app::fail(APP_NAME, EXIT_NO_SEAT, "seat acquire refused");
        }
        let code = session();
        // Owner-checked release on every exit path: a lease already lost
        // refuses (typed, ignored) — heal, never widen.
        //
        // A clean exit is a session the user ended, and the authority brings
        // the login screen back on this seat, so the screen is handed on
        // cleared. Any other exit is a failure whose reason belongs on the
        // console, which therefore takes the screen back.
        let next = if code == 0 {
            ReleaseSurface::Handover
        } else {
            ReleaseSurface::Text
        };
        let _ = tairix_rt::display_release(SEAT_PRIMARY, next);
        code
    }

    tairix_rt::entry!(main);
}

// --- Host stub ----------------------------------------------------------
//
// On the host (`cargo build --workspace`, clippy, fmt) the program's real
// entry — the freestanding `tairix-rt` `_start` path — is not compiled, so
// this inert `main` keeps the crate building under the host tooling. It
// performs no I/O.
#[cfg(not(freestanding))]
fn main() {}

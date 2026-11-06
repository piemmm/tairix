//! The `terminal.app` bundle's `Run` entry point (`plans/APPWIN.md` AW4,
//! `plans/GUI-TERMINAL.md`): the windowed terminal emulator hosting the
//! user's shells over the desktop session's window channel.
//!
//! # One process, many windows
//!
//! A terminal window is not an application: the application is the emulator,
//! and each of its windows is one hosted shell. So this is **one process
//! with a `Vec` of windows** — each carrying its own pseudo-terminal, shell
//! child, screen model, retained picture, and look — and the one Settings
//! window, parked on one wait-set that carries one event mailbox for the whole process plus that
//! window's own shell-output and child members. Opening another window costs
//! a pty, a spawn, a frame region, and two wait-set members; it costs no
//! second process, no second event mailbox, and no second icon-bar slot.
//!
//! That is what makes the desktop's icon bar honest. The bar shows
//! applications, so the terminal declares **one** presence
//! ([`tairix_terminal::appbar`]) whose slot stands for the emulator: a
//! primary click on it opens a fresh window, its menu offers *New window*
//! and *Quit*, and hovering it picks between the windows it owns. Closing
//! the last window puts the terminal away rather than ending it — the slot
//! stays, and a click there opens the next; only *Quit* closes them all and
//! ends the process.
//!
//! # What the program wires (and what stays in the libraries)
//!
//! Everything with behaviour worth testing lives in host-tested crates —
//! the screen model and its `lib/vt`-consuming parser (`tairix_terminal`),
//! the retained cell renderer (`tairix_terminal::render`), the user's profile
//! and its document, the look each window is drawn with
//! (`tairix_terminal::look`), the screen-effect pipeline, the right-click
//! menu, the settings sheet, the spawned shell's pipe wiring, the icon-bar
//! declaration, and the window channel's client half (`tairix_window`). This
//! binary only composes them over the live syscalls:
//!
//! * Per window, two ends of one kernel pseudo-terminal: keystrokes flow to
//!   that shell's standard input and its cooked output flows back to that
//!   screen. The child-side end is closed here after the spawn, so the shell
//!   observes end-of-file the moment its window goes, and the window
//!   observes end-of-stream the moment the shell does.
//! * Per window, one `shm_create`d frame region granted to the reserved
//!   window endpoint (the zero-copy surface the session maps once at
//!   create).
//! * One wait-set the program **parks** on — never a poll loop — carrying
//!   its `port_bind`-bound event mailbox (every window's deliveries and the
//!   application-scoped icon-bar events, each accepted only from the session
//!   identity the squat-protected create reply named), one shell-output and
//!   one child member per window, the memory-pressure wake, and the settings
//!   worker's wake. When an animated screen effect is in force the park
//!   carries a one-shot frame deadline, so the animation costs one timed wake
//!   per frame and nothing at all when it is switched off.
//! * The user's own profile document, read at start-up under this process's
//!   own identity and rewritten on a worker thread whenever a *settled* edit
//!   asks for it. It is the *user's* profile, so every window shares it, and
//!   the loop never waits on the store: a slider drag applies live and asks
//!   for one write when it ends (`tairix_terminal::publish`).
//!
//! A key press is either a terminal command (a menu accelerator, or input
//! the open menu or settings sheet owns) or shell input encoded through the
//! one shared `lib/keymap` rule. Every bring-up refusal exits fail-loud with
//! a reserved code and a stated reason on `stderr`.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy,
//! and fmt still cover the file.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

// --- Pure-Rust program --------------------------------------------------
#[cfg(freestanding)]
mod program {
    extern crate alloc;

    use alloc::boxed::Box;
    use alloc::vec::Vec;

    use tairix_abi::latency::DEFAULT_FRAME_BUDGET_NS;

    use tairix_abi::driver::display::DamageRect;
    use tairix_abi::window_ipc::{
        AppMenuItemId, DialogModality, MenuOutcome, PointerAction, WindowEvent, WindowRegion,
    };
    use tairix_abi::{Errno, ProcId, Signal, WaitSetOp, WaitSourceKind};
    use tairix_geometry::{Point, Rect, Scale};
    use tairix_input::InputEvent;
    use tairix_keymap::{encode_key_input, MAX_KEY_BYTES};
    use tairix_parallel::{JobRunner, Pool};
    use tairix_terminal::appbar::{self, BarCommand};
    use tairix_terminal::effects::{frame_due, frame_interval_ns};
    use tairix_terminal::layout::{grid_dims, grid_size, snap_to_cells, window_size};
    use tairix_terminal::menu::{self, Command};
    // `Settings` here is the sheet UI; the app-data handle is `SettingsStore`.
    use tairix_appdata::{Opening, Publication, PublishWorker, RtHost, Settings as SettingsStore};
    use tairix_soundtheme::cue::RtCuePost;
    use tairix_soundtheme::{Answer, PostedCues, SoundEvent, CUE_ANSWER_DEADLINE_NS};
    use tairix_terminal::profile::{Invalidation, Profile};
    use tairix_terminal::render::Screen;
    use tairix_terminal::settings::{preferred_extent, Settings, SheetOutcome, SETTINGS_TITLE};
    use tairix_terminal::sheet::SheetScreen;
    use tairix_terminal::{
        shell_env, shell_reap, shell_wires, win_sizing, EndingShell, EndingShells, Reshaped,
        ShellReap, ShellSource, Terminal, TERM,
    };
    use tairix_theme::{Theme, ThemeRegistry};
    use tairix_users::DEFAULT_SHELL;
    use tairix_window::app::{self, Wake, WindowPane};
    use tairix_window::{
        damage_in, key_input_event, pointer_input_events, pointer_point, present_damage_list,
        scroll_input_events, Desktop, EventError, EventMailbox, Repaint, WindowClient,
        WindowEvents, WindowTransport,
    };

    /// The wait-set token of the settings worker's wake pipe: readable exactly
    /// when a publish has answered, so the profile the store now holds is
    /// adopted through the park the loop is already in rather than by polling.
    const PUBLISH_TOKEN: u64 = app::FIRST_APP_TOKEN;

    /// The wait-set token of the sound authority's answer to a bell.
    const BELL_TOKEN: u64 = PUBLISH_TOKEN + 1;

    /// The shell's bell, heard through the cue authority without ever waiting
    /// on it.
    struct Bell {
        cues: PostedCues,
        post: RtCuePost,
    }

    impl Bell {
        fn new(set: u64) -> Self {
            Self {
                cues: PostedCues::new(),
                post: RtCuePost::new(set, BELL_TOKEN),
            }
        }

        /// Sound the bell if `open`'s shell rang it: refused or silent, the
        /// answer is the authority's to record.
        fn ring_if_rung(&mut self, open: &mut TerminalWindow) {
            if open.terminal.take_bell() {
                let _ = self.cues.post(
                    &mut self.post,
                    SoundEvent::Bell,
                    Answer::Started,
                    CUE_ANSWER_DEADLINE_NS,
                );
            }
        }

        fn collect(&mut self) {
            self.cues.collect(&mut self.post, |_, _| {});
        }
    }

    /// Where the per-window token pairs begin, clear of the fixed tokens
    /// above.
    ///
    /// Window `slot` owns `WINDOW_TOKEN_BASE + slot * 2` for its
    /// shell-output stream and the value after it for its shell child. The
    /// slot is a monotonic counter this process mints, never an index into a
    /// list that shifts, so a token names the same window for as long as
    /// that window lives and is never reused after it goes.
    const WINDOW_TOKEN_BASE: u64 = 16;

    /// Reap a hosted shell if it has exited.
    ///
    /// The shell's exit becomes visible on both the output-stream member
    /// (end-of-stream, as its write ends close) and the child member, and the
    /// wait-set may wake on either first, so every arm funnels through this
    /// one reap and a load-failure diagnosis is never lost to whichever token
    /// woke the loop.
    fn reap_shell(shell_pid: i64) -> ShellReap {
        shell_reap(tairix_rt::try_reap(shell_pid))
    }

    /// State why a reaped shell never got off the ground, if it did not.
    fn report_launch_failure(reason: Option<&'static str>) {
        if let Some(reason) = reason {
            app::report(OWN_WORD, format_args!("shell failed to launch: {reason}"));
        }
    }

    /// Take a reaped shell's child member off the wait-set, if it had one.
    fn forget(set: u64, shell: EndingShell) {
        if let Some(token) = shell.token {
            let _ = tairix_rt::waitset_ctl(
                set,
                WaitSetOp::Del,
                WaitSourceKind::Child,
                #[allow(clippy::cast_sign_loss)] // A PID, known non-negative.
                {
                    shell.pid as u64
                },
                token,
            );
        }
    }

    /// The command word this application's bundle is installed under, which
    /// selects nothing but its own shipped defaults: the store itself is keyed
    /// on the bundle identity the kernel attests.
    const OWN_WORD: &str = "terminal";

    /// The profile in force for this user, and a note for every stored
    /// setting the registry refused.
    ///
    /// A store the app-data service cannot serve — no service bound, a volume
    /// still to be unlocked — leaves the bundle's shipped defaults standing
    /// and is reported once, rather than running on settings whose provenance
    /// the user cannot see. A key the registry refuses costs only itself and
    /// is named.
    fn load_profile(settings: &SettingsStore<'_>) -> Profile {
        let (profile, refusals) = tairix_appdata::loaded(settings);
        for refusal in refusals {
            app::report(OWN_WORD, refusal);
        }
        profile
    }

    /// The settings sheet and the window it is drawn in.
    ///
    /// The terminal has one, a modeless dialog of the window it was opened
    /// from: every event delivered for its own window id routes to it, and
    /// the terminal windows go on taking input while it is up.
    struct SettingsWindow {
        /// The sheet itself. Boxed because it dwarfs everything around it,
        /// so the one allocation happens when Settings opens.
        sheet: Box<Settings>,
        /// The sheet's picture, retained between frames, and the rectangle it
        /// owes the next paint. Every edit reports into this and repaints only
        /// what it reported.
        picture: SheetScreen,
        /// Its channel-side state, whose geometry is the viewport the sheet
        /// is laid out and hit-tested in.
        pane: WindowPane,
        /// The terminal window it is a dialog of, which takes it down with it.
        owner: u64,
        /// Set once the sheet has asked to go. The loop closes the window;
        /// the routing itself holds no window client.
        dismissed: bool,
        /// Its paint waits for the answer to its last frame, and is given
        /// before the next park once that lands.
        owed: bool,
    }

    impl SettingsWindow {
        /// Bring the sheet's own copy up to `profile` wherever it has fallen
        /// behind — a store answer, an edit made in another window or from the
        /// menu — reporting the rows that change.
        fn follow(&mut self, profile: &Profile, theme: &Theme, scale: Scale) {
            if self.sheet.profile() != profile {
                let viewport = self.viewport();
                self.sheet
                    .adopt(*profile, viewport, scale, theme, self.picture.sink());
            }
        }

        /// The viewport the sheet occupies: the window's whole client.
        fn viewport(&self) -> Rect {
            let mode = self.pane.mode();
            Rect::new(0, 0, mode.width_px, mode.height_px)
        }

        /// Fit the window to a `width` × `height` client: what the session
        /// refit it to, or what a new scale wants. A new size owes a whole
        /// paint, and one the picture or the session cannot take keeps the
        /// current one, which the sheet scrolls within; the size in force owes
        /// nothing.
        fn refit(
            &mut self,
            client: &mut WindowClient<app::RtWindowTransport>,
            (width, height): (u32, u32),
        ) {
            let mode = *self.pane.mode();
            if (width, height) == (mode.width_px, mode.height_px) {
                return;
            }
            let Some(picture) = SheetScreen::new(width, height) else {
                return;
            };
            if self.pane.resize(client, &app::mode_for(width, height)) == app::Fitted::Adopted {
                self.picture = picture;
            }
        }

        /// Close the window; its frame region is unmapped by its own drop.
        fn close(self, client: &mut WindowClient<app::RtWindowTransport>) {
            let _ = self.pane.close(client);
        }
    }

    /// Close the Settings window when it is a dialog of terminal window
    /// `owner`, which is closing; answers whether it was, since closing it
    /// settles what it last showed however it closes.
    fn close_settings_of(
        settings: &mut Option<SettingsWindow>,
        owner: u64,
        client: &mut WindowClient<app::RtWindowTransport>,
    ) -> bool {
        let Some(held) = settings.take_if(|held| held.owner == owner) else {
            return false;
        };
        held.close(client);
        true
    }

    /// The client extent the Settings window wants at `desktop`'s scale,
    /// capped by its screen.
    fn settings_extent(desktop: &Desktop) -> (u32, u32) {
        let screen = desktop.screen();
        let (width, height) = preferred_extent(desktop.scale());
        (width.min(screen.width), height.min(screen.height))
    }

    /// Ask the desktop to open this window's menu at the press that asked
    /// for it.
    ///
    /// The anchor is the client-local point the press was reported at, which
    /// is the only space the terminal can speak truthfully: it is never told
    /// where its window sits. The desktop places, draws, grabs and dismisses;
    /// the answer arrives later as one `MenuClosed` naming the id minted
    /// here. A refusal is an answer — it is reported and the terminal carries
    /// on with no menu, never drawing one of its own.
    fn open_window_menu(
        client: &mut WindowClient<app::RtWindowTransport>,
        open: &mut TerminalWindow,
        at: Point,
    ) {
        let model = match menu::model() {
            Ok(model) => model,
            Err(err) => {
                app::report(
                    OWN_WORD,
                    format_args!("menu model refused ({err}); not shown"),
                );
                return;
            }
        };
        let anchor = match WindowRegion::new(at.x, at.y, 0, 0) {
            Ok(anchor) => anchor,
            Err(err) => {
                app::report(
                    OWN_WORD,
                    format_args!("menu anchor refused ({err}); not shown"),
                );
                return;
            }
        };
        match client.open_menu(open.pane.id(), anchor, &model) {
            Ok(open_id) => open.menu = Some(open_id),
            Err(err) => app::report(OWN_WORD, format_args!("menu refused ({err}); not shown")),
        }
    }

    /// Open the Settings window over `sheet` as a modeless dialog of
    /// terminal window `owner`, at the extent the sheet wants, drawn once.
    ///
    /// `None` — with the reason already on `stderr` — opens nothing, and the
    /// terminal carries on.
    fn open_settings(
        client: &mut WindowClient<app::RtWindowTransport>,
        (owner, server): (u64, ProcId),
        event_endpoint: u64,
        sheet: Box<Settings>,
        theme: &Theme,
        desktop: &Desktop,
    ) -> Option<SettingsWindow> {
        let (width, height) = settings_extent(desktop);
        if width == 0 || height == 0 {
            app::report(
                OWN_WORD,
                "settings window has no drawable extent; not shown",
            );
            return None;
        }
        let Some(picture) = SheetScreen::new(width, height) else {
            app::report(
                OWN_WORD,
                "settings window picture could not be allocated; not shown",
            );
            return None;
        };
        let pane = match WindowPane::open_dialog(
            client,
            (owner, server),
            event_endpoint,
            &app::mode_for(width, height),
            DialogModality::Modeless,
            SETTINGS_TITLE,
        ) {
            Ok(pane) => pane,
            Err(err) => {
                app::report(OWN_WORD, format_args!("{err}; settings window not shown"));
                return None;
            }
        };
        let mut window = SettingsWindow {
            sheet,
            picture,
            pane,
            owner,
            dismissed: false,
            owed: false,
        };
        if present_settings(&mut window, theme, desktop.scale(), client).is_err() {
            app::report(OWN_WORD, "settings window present refused; not shown");
            window.close(client);
            return None;
        }
        Some(window)
    }

    /// Paint whatever the Settings window owes and present exactly that
    /// rectangle, once the answer to its last frame is in: until then the
    /// paint waits, its damage kept, so the loop never waits for the desktop.
    /// A region the session released holds none of the pixels a partial
    /// present would leave standing, so it is drawn whole instead.
    fn present_settings(
        window: &mut SettingsWindow,
        theme: &Theme,
        scale: Scale,
        client: &mut WindowClient<app::RtWindowTransport>,
    ) -> Result<(), Errno> {
        window.owed = client.present_in_flight(window.pane.id());
        if window.owed {
            return Ok(());
        }
        if window.pane.content_released() {
            window.picture.invalidate();
        }
        let viewport = window.viewport();
        let painted = window.picture.paint(&window.sheet, viewport, scale, theme);
        if painted.is_empty() {
            return Ok(());
        }
        let Some(damage) = damage_in(window.pane.mode(), painted) else {
            // Nothing of the paint survives the clip, so these pixels were
            // never shown: cover the sheet next time rather than leave the
            // picture silently ahead of the screen.
            window.picture.invalidate();
            return Ok(());
        };
        window
            .pane
            .present(client, window.picture.surface(), damage)
    }

    /// One window's shell channel: the master end of its own kernel
    /// pseudo-terminal.
    ///
    /// A named type rather than a pair of closures, because every window
    /// holds one and they all live in the same list — a closure's type is its
    /// own, so a list of them could not be built.
    struct PtyShell {
        /// The pty master descriptor. Reads drain the shell's cooked output;
        /// writes feed keystrokes through the input discipline.
        master: u32,
    }

    impl ShellSource for PtyShell {
        fn read(&mut self) -> Result<Vec<u8>, Errno> {
            let mut chunk = [0u8; 4096];
            let read =
                tairix_rt::fs_read(self.master, 0, &mut chunk).map_err(Errno::from_syscall)?;
            match chunk.get(..read) {
                Some(slice) => Ok(slice.to_vec()),
                None => Err(Errno::OutOfRange),
            }
        }

        fn write(&mut self, bytes: &[u8]) -> Result<(), Errno> {
            let mut sent = 0;
            while sent < bytes.len() {
                let slice = bytes.get(sent..).ok_or(Errno::OutOfRange)?;
                let wrote =
                    tairix_rt::fs_write(self.master, 0, slice).map_err(Errno::from_syscall)?;
                if wrote == 0 {
                    return Err(Errno::WouldBlock);
                }
                sent += wrote;
            }
            Ok(())
        }
    }

    /// How a window's screen currently looks, its passes spread across the
    /// process's one pool.
    type Look = tairix_terminal::Look<&'static EffectPool>;

    /// The worker pool the screen effects spread across, started the first
    /// time a pass draws, so a terminal with the effects off holds no worker
    /// threads.
    ///
    /// The CPU count is asked once at start-up rather than here, because the
    /// question is a call to another service and this is reached from the
    /// loop that owes the windows their frames.
    struct EffectPool {
        online: usize,
        pool: core::cell::OnceCell<Pool>,
    }

    impl EffectPool {
        fn runner(&self) -> &dyn JobRunner {
            self.pool
                .get_or_init(|| app::pool_across(OWN_WORD, self.online, "drawing screen effects"))
        }
    }

    /// One terminal window: its hosted shell, its screen, and everything
    /// drawn for it.
    ///
    /// Each window is independent of its siblings in every way but the
    /// user's profile (which is the *user's*, not a window's) and the one
    /// event mailbox they share. So a shell exiting or a resize reaches
    /// exactly one of them.
    struct TerminalWindow {
        /// Its channel-side state: the window-channel id its events arrive
        /// under, its shared frame region, and the geometry both are shaped
        /// as.
        pane: WindowPane,
        /// This process's slot for the window, naming its two wait-set
        /// tokens.
        slot: u64,
        /// The pty master descriptor, kept beside the screen model so a
        /// resize can tell the shell its new window size.
        pty_master: u32,
        /// The hosted shell's PID, for the reap.
        shell_pid: i64,
        /// The screen model over that shell.
        terminal: Terminal<PtyShell>,
        /// The retained window picture.
        screen: Screen,
        /// How this window's screen currently looks.
        look: Look,
        /// The open id of this window's unanswered menu, if one is up.
        ///
        /// The desktop mints one per gesture and never reuses it, so an
        /// answer that names anything else belongs to a gesture already
        /// settled and is not acted on.
        menu: Option<u64>,
        /// Whether the window owes a paint: an event the drain applied changed
        /// what it shows, painted once the drain is applied rather than once
        /// per event, or its screen moved while its last frame was unanswered,
        /// painted before the next park once that answer lands.
        unpainted: bool,
    }

    impl TerminalWindow {
        /// The wait-set token of this window's shell-output stream member.
        const fn shell_token(&self) -> u64 {
            WINDOW_TOKEN_BASE + self.slot * 2
        }

        /// The wait-set token of this window's shell-child member.
        const fn child_token(&self) -> u64 {
            WINDOW_TOKEN_BASE + self.slot * 2 + 1
        }

        /// Bring this window's picture up to date and present what changed,
        /// the screen first taking the size last asked of it — or, while the
        /// window's last frame is unanswered, owe it the paint: the desktop
        /// answers once it shows that frame, and the loop never waits for it,
        /// so the shell's output is read as fast as it parses.
        ///
        /// A region the session released while the window was hidden is
        /// re-attached first, so this paints into a live one.
        fn present(
            &mut self,
            client: &mut WindowClient<app::RtWindowTransport>,
        ) -> Result<(), Errno> {
            if client.present_in_flight(self.pane.id()) {
                self.unpainted = true;
                return Ok(());
            }
            self.settle();
            self.unpainted = false;
            present_frame(
                &self.terminal,
                &mut self.look,
                &mut self.screen,
                client,
                &mut self.pane,
            )
        }

        /// Give the screen the size last asked of it, telling the shell a size
        /// only once the screen has taken it.
        fn settle(&mut self) {
            match self.terminal.settle() {
                Reshaped::Unchanged => {}
                Reshaped::Took { cols, rows } => {
                    let _ = tairix_rt::pty_set_size(self.pty_master, rows, cols);
                }
                Reshaped::Refused => app::report(
                    OWN_WORD,
                    "the screen could not take its window's new size; it keeps the one it had",
                ),
            }
        }

        /// Bring this window up to what `changed` made stale — the backdrop
        /// blur, the grid, the retained picture, the scrollback's limit — and
        /// present it if its pixels moved.
        ///
        /// The one path a profile change and a desktop change both reach a
        /// window by, so each costs the window only what it moved.
        fn restyle(
            &mut self,
            changed: Invalidation,
            profile: &Profile,
            client: &mut WindowClient<app::RtWindowTransport>,
        ) -> Result<(), Errno> {
            if changed.blur() {
                apply_blur(client, self.pane.id(), profile);
            }
            if changed.metrics() {
                let mode = *self.pane.mode();
                let (cols, rows) = grid_dims(mode.width_px, mode.height_px, self.look.font);
                self.terminal.resize(cols, rows);
            }
            if changed.metrics() || changed.painted() {
                // A new face or new colours: every retained pixel is stale,
                // and the cell diff can see neither.
                self.screen.invalidate();
            }
            // Lines let go under a view scrolled back move what it shows.
            let shed = changed.scrollback()
                && self
                    .terminal
                    .set_scrollback_limit(usize::from(profile.scrollback_lines));
            if changed.repaints() || shed {
                self.present(client)?;
            }
            Ok(())
        }

        /// Fit the window to the client the session granted, snapped down to
        /// whole cells so no partial-cell strip is left at its right or bottom
        /// edge: the frame region is re-mapped now, and the screen takes its
        /// grid when the window is next painted, so a burst of samples costs
        /// one reflow. Snapping is idempotent, so the `Resized` a re-map draws
        /// back snaps to the size in force and changes nothing; a size the
        /// session or the region cannot take keeps the current one.
        fn refit(
            &mut self,
            client: &mut WindowClient<app::RtWindowTransport>,
            (width_px, height_px): (u32, u32),
        ) {
            let (width, height) = snap_to_cells(width_px, height_px, self.look.font);
            let current = *self.pane.mode();
            if (width, height) == (current.width_px, current.height_px) {
                return;
            }
            if self.pane.resize(client, &app::mode_for(width, height)) == app::Fitted::Adopted {
                let (cols, rows) = grid_dims(width, height, self.look.font);
                self.terminal.resize(cols, rows);
                // What the passes remember is the shape of the old screen; a
                // resized one must not ghost it.
                self.look.pipeline.clear();
                self.unpainted = true;
            }
        }

        /// Close this window and its pty master, handing back its shell child;
        /// the frame region is unmapped by its own drop.
        ///
        /// The master is closed here rather than left to process exit,
        /// because a process that keeps running must not hold a dead
        /// window's descriptors.
        fn release(
            self,
            client: &mut WindowClient<app::RtWindowTransport>,
            set: u64,
        ) -> EndingShell {
            // Read before the pane is consumed, which moves it out of `self`.
            let shell = EndingShell {
                token: Some(self.child_token()),
                pid: self.shell_pid,
            };
            let (shell_token, pty_master) = (self.shell_token(), self.pty_master);
            let _ = self.pane.close(client);
            let _ = tairix_rt::waitset_ctl(
                set,
                WaitSetOp::Del,
                WaitSourceKind::Stream,
                u64::from(pty_master),
                shell_token,
            );
            let _ = tairix_rt::fs_close(pty_master);
            shell
        }

        /// Close a window whose shell has been reaped.
        fn close(self, client: &mut WindowClient<app::RtWindowTransport>, set: u64) {
            forget(set, self.release(client, set));
        }

        /// Close a window whose shell has not been reaped, and end the shell.
        ///
        /// Ending it ends every job it started, which live in its session.
        /// Its child member stays on the wait-set, so its exit wakes the loop
        /// to reap it from `ending`.
        fn hang_up(
            self,
            client: &mut WindowClient<app::RtWindowTransport>,
            set: u64,
            ending: &mut EndingShells,
        ) {
            let shell = self.release(client, set);
            // Refused only once the shell is already exiting, which is reaped
            // the same way.
            let _ = tairix_rt::signal(shell.pid, Signal::Terminate);
            ending.push(shell);
        }

        /// Close a window that never finished opening, and end the shell
        /// spawned for it. One whose exit the wait-set could not be made to
        /// report is killed outright and reaped on the loop's next wake.
        fn abandon(
            self,
            client: &mut WindowClient<app::RtWindowTransport>,
            set: u64,
            ending: &mut EndingShells,
            watched: bool,
        ) {
            if watched {
                self.hang_up(client, set, ending);
                return;
            }
            let shell = EndingShell {
                token: None,
                ..self.release(client, set)
            };
            let _ = tairix_rt::signal(shell.pid, Signal::Kill);
            ending.push(shell);
        }
    }

    /// Everything the bring-up of one window needs from the process it joins.
    struct WindowContext<'a> {
        /// The window channel.
        client: &'a mut WindowClient<app::RtWindowTransport>,
        /// The one event mailbox every window reports through.
        event_endpoint: u64,
        /// The session the desktop query named, the one sender a window
        /// create's reply may come from.
        server: ProcId,
        /// The wait-set the process parks on.
        set: u64,
        /// The slot this window takes, naming its two tokens.
        slot: u64,
        /// The user's profile, shared by every window.
        profile: &'a Profile,
        /// The active theme.
        theme: &'a Theme,
        /// The desktop the window opens on.
        desktop: &'a Desktop,
        /// This terminal's inherited environment, forwarded to the shell.
        env: &'a [Vec<u8>],
        /// Where the shell of a window that fails to open is left to be reaped.
        ending: &'a mut EndingShells,
        /// How many lines its scrollback keeps.
        scrollback: usize,
        /// The workers its screen effects spread across.
        pool: &'static EffectPool,
    }

    /// One window's pseudo-terminal, the shell hosted on it, and the screen
    /// model over the master end.
    struct HostedShell {
        /// The screen model, driven by the shell's cooked output.
        terminal: Terminal<PtyShell>,
        /// The pty master descriptor, kept so a resize can tell the shell its
        /// new window size and a close can release it.
        pty_master: u32,
        /// The hosted shell's PID, for the reap.
        pid: i64,
    }

    /// Create a pty at `cols` × `rows`, build the screen model over its
    /// master end, and spawn the shell on its slave.
    ///
    /// The pty is created at the grid the window will actually show, so
    /// `terminal_size` reports it, and the shell's fd 0/1/2 are the slave — a
    /// console-class tty — so it runs its full interactive editor exactly as
    /// on the hardware console (`plans/PTY.md`).
    ///
    /// The grid is allocated *before* the spawn, so a refused allocation costs
    /// no process load and leaves no shell behind to reap. Every refusal
    /// answers its reason and closes what it had opened, so the caller has only
    /// its window to take back down.
    fn open_shell(
        cols: u16,
        rows: u16,
        env: &[Vec<u8>],
        scrollback: usize,
    ) -> Result<HostedShell, &'static str> {
        let Ok((pty_master, pty_slave)) = tairix_rt::pty_create(rows, cols) else {
            return Err("no pseudo-terminal could be had for its shell");
        };
        let shell = PtyShell { master: pty_master };
        let Some(mut terminal) = Terminal::new(cols, rows, shell, scrollback) else {
            let _ = tairix_rt::fs_close(pty_master);
            let _ = tairix_rt::fs_close(pty_slave);
            return Err("there is not enough memory for its screen");
        };
        // Held to the band in force from its first line, not from the next
        // change.
        terminal.shed_for(tairix_rt::pressure::gauge().band());
        let attach = shell_wires(pty_slave);
        let env: Vec<&[u8]> = env.iter().map(Vec::as_slice).collect();
        let pid = tairix_rt::spawn_attached(DEFAULT_SHELL.as_bytes(), &attach, &[b"elsh"], &env);
        // Close this process's own slave end either way: the spawn cloned it
        // into the shell, and keeping it here would mask the shell's exit.
        let _ = tairix_rt::fs_close(pty_slave);
        if pid < 0 {
            let _ = tairix_rt::fs_close(pty_master);
            return Err("its shell could not be started");
        }
        Ok(HostedShell {
            terminal,
            pty_master,
            pid,
        })
    }

    /// Open one terminal window: its screen model and retained picture, the
    /// desktop window and its shared frame region, and then its
    /// pseudo-terminal, hosted shell, and two wait-set members.
    ///
    /// The desktop is asked for the window before anything is spawned, so a
    /// session that refuses one costs a single round trip rather than a pty
    /// and a whole shell process brought up and torn straight back down.
    ///
    /// Returns the window, or why it could not be opened. Every refusal
    /// unwinds what it had allocated, so a window that could not be opened
    /// leaves nothing mapped and no member of its own on the wait-set, and a
    /// shell already spawned for it is ended and reaped as a closed window's
    /// is.
    #[allow(clippy::needless_pass_by_value)] // The context is a bundle of borrows, moved so the caller cannot reuse a stale one.
    fn open_window(ctx: WindowContext<'_>) -> Result<TerminalWindow, &'static str> {
        let look = Look::resolve(ctx.profile, ctx.theme, ctx.desktop, ctx.pool);
        let output = (
            ctx.desktop.screen_width_px(),
            ctx.desktop.screen_height_px(),
        );
        let (w, h) = window_size(look.font, output, ctx.theme, ctx.desktop.scale());
        let (cols, rows) = grid_dims(w, h, look.font);

        let Some(screen) = Screen::new(w, h) else {
            return Err("there is not enough memory to draw it");
        };

        let mode = app::mode_for(w, h);

        // The desktop is asked *before* a pty is created or a shell spawned,
        // because the session can refuse (it bounds the windows one client
        // may hold) and a refusal must cost nothing: spawning a shell that is
        // immediately thrown away is a whole process load and teardown per
        // click, which is felt right across the desktop when a user keeps
        // asking for windows they cannot have.
        //
        // A character grid can show nothing at all below one whole cell, and
        // that is where the terminal's own snap to whole cells bottoms out,
        // so one cell of the face it opens in is its declared floor.
        let (min_width_px, min_height_px) = grid_size(1, 1, look.font);
        let pane = match WindowPane::open(
            ctx.client,
            ctx.event_endpoint,
            &mode,
            "Terminal",
            win_sizing(min_width_px, min_height_px),
        ) {
            Ok((pane, replied)) if replied == ctx.server => pane,
            // Something else answering for the window endpoint: the window it
            // named is closed rather than drawn into.
            Ok((pane, _)) => {
                let _ = pane.close(ctx.client);
                return Err("a window reply came from another sender");
            }
            Err(err) => {
                app::report(
                    OWN_WORD,
                    format_args!("the desktop refused a window: {err}"),
                );
                return Err("the desktop would not open it");
            }
        };

        let shell = match open_shell(cols, rows, ctx.env, ctx.scrollback) {
            Ok(shell) => shell,
            Err(why) => {
                let _ = pane.close(ctx.client);
                return Err(why);
            }
        };

        let mut opened = TerminalWindow {
            pane,
            slot: ctx.slot,
            pty_master: shell.pty_master,
            shell_pid: shell.pid,
            terminal: shell.terminal,
            screen,
            look,
            menu: None,
            unpainted: false,
        };
        // The child member first, so from here every failure can hand the
        // shell over to be reaped when it exits.
        let members = [
            (
                WaitSourceKind::Child,
                #[allow(clippy::cast_sign_loss)] // A PID, known non-negative.
                {
                    opened.shell_pid as u64
                },
                opened.child_token(),
            ),
            (
                WaitSourceKind::Stream,
                u64::from(opened.pty_master),
                opened.shell_token(),
            ),
        ];
        for (watched, (kind, id, token)) in members.into_iter().enumerate() {
            if tairix_rt::waitset_ctl(ctx.set, WaitSetOp::Add, kind, id, token) != 0 {
                opened.abandon(ctx.client, ctx.set, ctx.ending, watched > 0);
                return Err("its shell could not be watched");
            }
        }
        apply_blur(ctx.client, opened.pane.id(), ctx.profile);
        if opened.present(ctx.client).is_err() {
            opened.abandon(ctx.client, ctx.set, ctx.ending, true);
            return Err("the desktop would not show it");
        }
        Ok(opened)
    }

    /// The headline of an alert saying a terminal window could not be opened.
    const NO_WINDOW: &str = "No terminal window could be opened";

    /// Bring the retained screen up to date, copy what changed into `frame`,
    /// and present it.
    ///
    /// **Only what changed is drawn, copied, and presented.** A keystroke
    /// costs the cell it wrote and the two the cursor moved between, plus the
    /// glow's reach around them when one is on; a step of a moving effect
    /// costs what that step moved. A wake that changed nothing presents
    /// nothing at all, so it costs the session no composite either. The
    /// passes draw into the pipeline's own stages, never the retained screen,
    /// so the next frame's diff still describes the text rather than the
    /// effect's own churn.
    ///
    /// The Settings window is *not* drawn here: it is a window of its own, so
    /// the screen effects can never wobble a settings control.
    fn present_frame<S, T>(
        terminal: &Terminal<S>,
        look: &mut Look,
        screen: &mut Screen,
        client: &mut WindowClient<T>,
        pane: &mut WindowPane,
    ) -> Result<(), Errno>
    where
        S: ShellSource,
        T: WindowTransport,
    {
        // The pane's mode is the one truth about the window's extent, so the
        // picture is reconciled to it here rather than at each site that
        // changes it: a surface and a frame region of different shapes cannot
        // arise.
        let mode = *pane.mode();
        let shaped = screen.surface().width() == mode.width_px
            && screen.surface().height() == mode.height_px;
        if !shaped && !screen.resize(mode.width_px, mode.height_px) {
            return Err(Errno::LengthOutOfRange);
        }
        let damage = screen.paint(terminal.grid(), &look.painted, look.font);
        let (_, passes) = look.effects.passes(look.scale.percent());
        if passes == 0 {
            if look.pipeline.retire() {
                return pane.present(client, screen.surface(), DamageRect::full(&mode));
            }
            if damage.is_empty() {
                return Ok(());
            }
            let Some(rect) = damage_in(&mode, damage) else {
                // The damage lies outside the window the session knows about,
                // so these pixels were never shown: repaint whole next frame
                // rather than leave the surface silently ahead of the screen.
                screen.invalidate();
                return Ok(());
            };
            return pane.present(client, screen.surface(), rect);
        }
        let Some(frame) = look.pipeline.render(
            screen.surface(),
            damage,
            look.effects,
            look.scale.percent(),
            look.phase,
            look.pool.runner(),
        ) else {
            // A refused buffer costs the effect, never the terminal: present
            // the plain screen rather than exiting over decoration.
            return pane.present(client, screen.surface(), DamageRect::full(&mode));
        };
        let Some(changed) = present_damage_list(&mode, Repaint::Reported, frame.changed) else {
            return Ok(());
        };
        pane.present_list(client, frame.picture, &changed)
    }

    /// Tell the session how far to blur what is behind this window.
    ///
    /// A refusal is reported and otherwise harmless: the window simply keeps
    /// the blur it already had, which is never worse than a sharp backdrop.
    fn apply_blur<T: WindowTransport>(
        client: &mut WindowClient<T>,
        window: u64,
        profile: &Profile,
    ) {
        if let Err(err) = client.set_backdrop_blur(window, profile.effects.blur_radius_px()) {
            app::report(OWN_WORD, format_args!("backdrop blur refused: {err}"));
        }
    }

    /// Program entry point. `tairix-rt`'s `_start` calls it once the
    /// runtime is set up and routes its return value through the `exit`
    /// syscall.
    #[allow(clippy::too_many_lines)] // One linear bring-up plus one event loop; splitting would obscure the teardown ordering.
    fn main() -> i32 {
        // From here this task drives a user-facing loop, so declare the
        // frame it owes. A debug image then reports any span that overruns,
        // naming the call that spent it; a shippable one arms nothing and
        // answers zero, which is why the result is not examined.
        let _ = tairix_rt::latency_watch(DEFAULT_FRAME_BUDGET_NS);
        // --- The user's own profile, before anything is sized or painted, so
        // the first frame is what they chose rather than a default corrected
        // once they have seen it. It is the user's, so every window shares
        // it.
        // Read on this task, unlike every later write: nothing is on screen
        // yet, so there is no frame to owe anyone.
        let mut publication = {
            let mut host = RtHost;
            Publication::new(load_profile(&SettingsStore::open_without_defaults(
                &mut host,
            )))
        };

        // Every later write goes to this worker instead, so a settled edit
        // costs the window no frame. It starts once the wait-set exists.
        let publisher = alloc::sync::Arc::new(tairix_appdata::publish_worker(Opening::Bare));
        let _publisher_guard = tairix_rt::work::WorkerGuard::new(&publisher);

        let pool: &'static EffectPool =
            alloc::boxed::Box::leak(alloc::boxed::Box::new(EffectPool {
                online: tairix_procinfo::online_cpus(&tairix_procinfo::IpcTransport),
                pool: core::cell::OnceCell::new(),
            }));

        // --- The desktop these windows will be shown on: the screen, the
        // density, and the appearance.
        let mut client = WindowClient::new(app::RtWindowTransport);
        let (mut desktop, mut themes) = match app::bring_up_desktop(&mut client) {
            Ok(brought_up) => brought_up,
            Err(err) => return app::fail(OWN_WORD, err.code(), err),
        };
        // The serving session's own identity, learned by the desktop query:
        // what authenticates its events and its window replies, with or
        // without a window open (fail closed).
        let Some(server) = client.session() else {
            return app::fail(
                OWN_WORD,
                app::EXIT_NO_WINDOW,
                "the desktop did not identify itself",
            );
        };

        // --- The one event mailbox and the wait-set the process parks on. One
        // mailbox serves every window and the icon bar.
        let binding = match app::bind_event_mailbox() {
            Ok(binding) => binding,
            Err(err) => return app::fail(OWN_WORD, err.code(), err),
        };
        let event_endpoint = binding.endpoint();
        let set = binding.set();
        app::start_worker(OWN_WORD, &publisher, set, PUBLISH_TOKEN, "settings");

        // Forward this terminal's own inherited environment (USER, HOME,
        // LOGNAME, PATH, LANG, ...) to every shell it hosts, exactly as the
        // desktop session forwards it to every app it launches: a hosted
        // shell is the logged-in user's shell, so its prompt and its children
        // need the same identity and locale the session runs under. The one
        // variable this terminal owns is TERM, naming the emulator it
        // presents, so its own value replaces any inherited TERM (the shared
        // `shell_env` rule, host-tested). The environment is data and carries
        // no authority.
        let env = shell_env(TERM, (0..tairix_rt::env_count()).filter_map(tairix_rt::env));

        // --- The icon-bar presence, before any window of this process
        // exists. A declared presence belongs to the *process*, so declaring
        // it first is what makes the slot carry this terminal's menu and its
        // primary-click action from the moment it appears; declare it after a
        // window and the session derives a slot from that window meanwhile —
        // one that opens no menu and does nothing when clicked.
        //
        // A refused declaration is an answer, not a death: the terminal
        // simply has no slot of its own and its windows are still reachable
        // through the one the session derives from them.
        if let Err(refused) =
            tairix_window::declare_app_bar(&mut client, appbar::declaration(event_endpoint))
        {
            app::report(OWN_WORD, format_args!("{refused}"));
        }

        // Shells whose windows have closed, each reaped once it has exited.
        let mut ending = EndingShells::new();

        // --- The first window. One refused is stated in an alert and the
        // terminal stays on its icon-bar slot, where a click asks again.
        let mut next_slot: u64 = 0;
        let mut windows: Vec<TerminalWindow> = Vec::new();
        let mut settings: Option<SettingsWindow> = None;
        match open_window(WindowContext {
            client: &mut client,
            event_endpoint,
            server,
            set,
            slot: next_slot,
            profile: publication.live(),
            theme: themes.active(),
            desktop: &desktop,
            env: &env,
            ending: &mut ending,
            scrollback: usize::from(publication.live().scrollback_lines),
            pool,
        }) {
            Ok(first) => {
                next_slot += 1;
                windows.push(first);
            }
            Err(why) => {
                let _ = app::refuse(
                    &mut client,
                    (OWN_WORD, event_endpoint),
                    None,
                    NO_WINDOW,
                    why,
                );
            }
        }

        // The one event stream for the whole process. It is held across
        // wakes because it owns the frame it reads ahead while folding a run
        // of resizes: dropping it between drains would lose that frame.
        let mut events = WindowEvents::new(EventMailbox::new(event_endpoint, server));

        // --- The event loop: park on the wait-set and dispatch on the woken
        // member's token (never drain every source per wake — a blocking
        // receive on an idle source would wedge the loop). Each member's
        // readiness is a level peek, so work left undrained re-reports on the
        // next wait. The park carries a frame deadline only while some window
        // has an animated effect in force.
        let mut bell = Bell::new(set);
        // When the next animation frame is due, while an effect is animated.
        let mut next_frame_ns: Option<u64> = None;
        let code = 'serve: loop {
            // A window owed a paint is painted before the park once its last
            // frame has been answered; while it has not, the answer is what
            // wakes the park.
            for open in windows.iter_mut().filter(|open| open.unpainted) {
                if open.present(&mut client).is_err() {
                    break 'serve app::fail(OWN_WORD, app::EXIT_CHANNEL_LOST, "present refused");
                }
            }
            if let Some(held) = settings.as_mut().filter(|held| held.owed) {
                let (theme, scale) = (themes.active(), desktop.scale());
                if present_settings(held, theme, scale, &mut client).is_err() {
                    break 'serve app::fail(
                        OWN_WORD,
                        app::EXIT_CHANNEL_LOST,
                        "settings present refused",
                    );
                }
            }
            let now = tairix_rt::clock_get();
            let animated = windows.iter().any(|open| {
                open.look
                    .pipeline
                    .animated(open.look.effects, open.look.scale.percent())
            });
            next_frame_ns = frame_due(animated, next_frame_ns, now, desktop.refresh());
            let held = settings
                .as_mut()
                .and_then(|held| held.sheet.repeat_deadline(now));
            let timeout = next_frame_ns
                .into_iter()
                .chain(held)
                .min()
                .map_or(u64::MAX, |due| due.saturating_sub(now));
            let woke = match app::park_for(set, timeout) {
                Ok(Some(wake)) => wake,
                Ok(None) => {
                    // A deadline elapsed: a held scrollbar press in the
                    // sheet, the animation's next frame, or both.
                    let now = tairix_rt::clock_get();
                    let (theme, scale) = (themes.active(), desktop.scale());
                    if let Some(held) = settings.as_mut() {
                        let viewport = held.viewport();
                        let stepped =
                            held.sheet
                                .repeat(now, viewport, scale, theme, held.picture.sink());
                        if stepped && present_settings(held, theme, scale, &mut client).is_err() {
                            break 'serve app::fail(
                                OWN_WORD,
                                app::EXIT_CHANNEL_LOST,
                                "settings present refused",
                            );
                        }
                    }
                    if next_frame_ns.is_some_and(|due| now >= due) {
                        next_frame_ns =
                            Some(now.saturating_add(frame_interval_ns(desktop.refresh())));
                        for open in &mut windows {
                            open.look.phase = open.look.phase.advance();
                            if open.present(&mut client).is_err() {
                                break 'serve app::fail(
                                    OWN_WORD,
                                    app::EXIT_CHANNEL_LOST,
                                    "present refused",
                                );
                            }
                        }
                    }
                    continue;
                }
                Err(_) => {
                    break 'serve app::fail(OWN_WORD, app::EXIT_CHANNEL_LOST, "wait-set lost")
                }
            };
            // A shell the wait-set could not watch is reaped on any wake.
            ending.reap(None, reap_shell, |shell, reason| {
                forget(set, shell);
                report_launch_failure(reason);
            });
            match woke {
                Wake::Event => {
                    let outcome = drain_events(
                        (&mut windows, &mut settings),
                        &mut publication,
                        &desktop,
                        themes.active(),
                        &mut events,
                        &mut client,
                    );
                    // A Settings window that has asked to go leaves before
                    // anything this same outcome opens.
                    if let Some(held) = settings.take_if(|held| held.dismissed) {
                        held.close(&mut client);
                    }
                    match apply_outcome(
                        outcome,
                        &mut windows,
                        &mut client,
                        AppContext {
                            set,
                            event_endpoint,
                            server,
                            next_slot: &mut next_slot,
                            publication: &mut publication,
                            themes: &mut themes,
                            desktop: &mut desktop,
                            publisher: &publisher,
                            env: &env,
                            ending: &mut ending,
                            settings: &mut settings,
                            pool,
                        },
                    ) {
                        Applied::Running => {}
                        Applied::Ended => break 'serve 0,
                        Applied::Lost(reason) => {
                            break 'serve app::fail(OWN_WORD, app::EXIT_CHANNEL_LOST, reason)
                        }
                    }
                }
                Wake::App(PUBLISH_TOKEN) => {
                    // The settings worker answered. Draining the nudge is the
                    // whole of noticing it; what it means is the one adopt
                    // path, so a publish the loop asked for and one it did
                    // itself land identically.
                    publisher.wake().drain();
                    match apply_outcome(
                        EventOutcome::ProfilePublished,
                        &mut windows,
                        &mut client,
                        AppContext {
                            set,
                            event_endpoint,
                            server,
                            next_slot: &mut next_slot,
                            publication: &mut publication,
                            themes: &mut themes,
                            desktop: &mut desktop,
                            publisher: &publisher,
                            env: &env,
                            ending: &mut ending,
                            settings: &mut settings,
                            pool,
                        },
                    ) {
                        Applied::Running => {}
                        Applied::Ended => break 'serve 0,
                        Applied::Lost(reason) => {
                            break 'serve app::fail(OWN_WORD, app::EXIT_CHANNEL_LOST, reason)
                        }
                    }
                }
                Wake::DesktopChanged => {
                    // The desktop belongs to the seat, not to one window, so
                    // it is adopted once and every window restyled in it. A
                    // refused state is stated and the last good one stands.
                    let changed = match app::adopt_desktop(&mut desktop, &mut themes) {
                        Ok(changed) => changed,
                        Err(err) => {
                            app::report(OWN_WORD, format_args!("desktop change refused: {err}"));
                            false
                        }
                    };
                    if changed {
                        match apply_outcome(
                            EventOutcome::DesktopChanged,
                            &mut windows,
                            &mut client,
                            AppContext {
                                set,
                                event_endpoint,
                                server,
                                next_slot: &mut next_slot,
                                publication: &mut publication,
                                themes: &mut themes,
                                desktop: &mut desktop,
                                publisher: &publisher,
                                env: &env,
                                ending: &mut ending,
                                settings: &mut settings,
                                pool,
                            },
                        ) {
                            Applied::Running => {}
                            Applied::Ended => break 'serve 0,
                            Applied::Lost(reason) => {
                                break 'serve app::fail(OWN_WORD, app::EXIT_CHANNEL_LOST, reason)
                            }
                        }
                    }
                }
                Wake::PressureChanged => {
                    tairix_font::trim_glyph_cache();
                    // The passes' buffers are whole screens of per-pixel
                    // state that only matter while an effect is on, so they
                    // give first under pressure; the next frame starts clean.
                    // The scrollback is held to the new band until it moves.
                    let band = tairix_rt::pressure::gauge().band();
                    for open in &mut windows {
                        open.look.pipeline.clear();
                        if open.terminal.shed_for(band) && open.present(&mut client).is_err() {
                            break 'serve app::fail(
                                OWN_WORD,
                                app::EXIT_CHANNEL_LOST,
                                "present refused",
                            );
                        }
                    }
                }
                Wake::PressureUnchanged => {}
                Wake::App(BELL_TOKEN) => bell.collect(),
                Wake::App(token) => {
                    // A per-window member — its shell wrote, or its shell
                    // exited — or the exit of a shell whose window has gone.
                    // Any other token is one this program never minted, so it
                    // simply re-parks.
                    if ending.watches(token) {
                        ending.reap(Some(token), reap_shell, |shell, reason| {
                            forget(set, shell);
                            report_launch_failure(reason);
                        });
                        continue;
                    }
                    let Some(index) = windows.iter().position(|open| {
                        open.shell_token() == token || open.child_token() == token
                    }) else {
                        continue;
                    };
                    let ended = if windows[index].shell_token() == token {
                        pump_shell(&mut windows[index], &mut client)
                    } else {
                        drain_and_reap(&mut windows[index], &mut client)
                    };
                    bell.ring_if_rung(&mut windows[index]);
                    // The shell this window hosted is gone, or going, so the
                    // window is: hosting it was the window's whole purpose.
                    // Its siblings keep running.
                    let owned = match ended {
                        ShellEnd::Running => false,
                        ShellEnd::Lost(reason) => {
                            break 'serve app::fail(OWN_WORD, app::EXIT_CHANNEL_LOST, reason)
                        }
                        ShellEnd::Exited(reason) => {
                            let closing = windows.remove(index);
                            let owned =
                                close_settings_of(&mut settings, closing.pane.id(), &mut client);
                            closing.close(&mut client, set);
                            report_launch_failure(reason);
                            owned
                        }
                        ShellEnd::OutputEnded => {
                            let closing = windows.remove(index);
                            let owned =
                                close_settings_of(&mut settings, closing.pane.id(), &mut client);
                            closing.hang_up(&mut client, set, &mut ending);
                            owned
                        }
                    };
                    // The Settings window closing with its owner settles what
                    // it last showed, as its own close does.
                    if owned {
                        match apply_outcome(
                            EventOutcome::ProfileChanged { settled: true },
                            &mut windows,
                            &mut client,
                            AppContext {
                                set,
                                event_endpoint,
                                server,
                                next_slot: &mut next_slot,
                                publication: &mut publication,
                                themes: &mut themes,
                                desktop: &mut desktop,
                                publisher: &publisher,
                                env: &env,
                                ending: &mut ending,
                                settings: &mut settings,
                                pool,
                            },
                        ) {
                            Applied::Running => {}
                            Applied::Ended => break 'serve 0,
                            Applied::Lost(reason) => {
                                break 'serve app::fail(OWN_WORD, app::EXIT_CHANNEL_LOST, reason)
                            }
                        }
                    }
                }
            }
        };
        see_out(&publisher, &mut publication);
        code
    }

    /// See the profile's writes out before the program ends, so the last
    /// change is never dropped with the worker: an edit still to settle is
    /// settled, and every write owed behind the one outstanding is made.
    fn see_out(publisher: &Publisher, publication: &mut Publication<Profile>) {
        let mut refusals = Vec::new();
        tairix_appdata::see_out_published(publisher, publication, &mut refusals);
        for refusal in refusals {
            app::report(OWN_WORD, refusal);
        }
    }

    /// What a window's shell channel wake concluded.
    enum ShellEnd {
        /// The shell is still running.
        Running,
        /// The shell was reaped; the terse reason to report, if it never got
        /// off the ground.
        Exited(Option<&'static str>),
        /// The shell's output ended before it could be reaped.
        OutputEnded,
        /// The channel itself failed: end the process fail-loud.
        Lost(&'static str),
    }

    /// Drain what `open`'s shell wrote and repaint that window, or owe it the
    /// paint while its last frame is unanswered.
    fn pump_shell(
        open: &mut TerminalWindow,
        client: &mut WindowClient<app::RtWindowTransport>,
    ) -> ShellEnd {
        match open.terminal.pump() {
            Ok(_) => {
                if open.present(client).is_err() {
                    return ShellEnd::Lost("present refused");
                }
                ShellEnd::Running
            }
            // End-of-stream: the shell exited (a clean `exit`, it was
            // killed, or — admitted by `spawn` but then unable to load its
            // own image — it failed asynchronously), or is on its way out.
            // What it last wrote is already on screen.
            Err(Errno::NotFound) => match reap_shell(open.shell_pid) {
                ShellReap::Gone(reason) => ShellEnd::Exited(reason),
                ShellReap::Running => ShellEnd::OutputEnded,
            },
            Err(_) => ShellEnd::Lost("shell channel lost"),
        }
    }

    /// Reap `open`'s exited shell and paint whatever output it left.
    fn drain_and_reap(
        open: &mut TerminalWindow,
        client: &mut WindowClient<app::RtWindowTransport>,
    ) -> ShellEnd {
        let ShellReap::Gone(reason) = reap_shell(open.shell_pid) else {
            return ShellEnd::Running;
        };
        while open.terminal.pump().is_ok() {}
        let _ = open.present(client);
        ShellEnd::Exited(reason)
    }

    /// The terminal's settings publisher: the store round trip a settled edit
    /// costs, carried out on a worker thread.
    ///
    /// The sheet's sliders are continuous, so a publish per value change is a
    /// publish per pointer-motion sample — an IPC round trip to the
    /// configuration service and a disk write each, with the window frozen for
    /// every one. The loop therefore *asks* and adopts nothing; the worker
    /// writes and answers with what the store then holds; the loop adopts that
    /// on the wake it nudges.
    type Publisher = PublishWorker<Profile>;

    /// Adopt whatever the publisher has answered with, state anything it could
    /// not use, submit the write owed behind it, and bring every window and
    /// sheet up to date.
    fn adopt_published(
        windows: &mut [TerminalWindow],
        client: &mut WindowClient<app::RtWindowTransport>,
        ctx: &mut AppContext<'_>,
    ) -> Applied {
        let mut refusals = Vec::new();
        tairix_appdata::collect_published(ctx.publisher, ctx.publication, &mut refusals);
        for refusal in refusals {
            app::report(OWN_WORD, refusal);
        }
        if apply_profile_change(windows, client, ctx).is_err() {
            return Applied::Lost("present refused");
        }
        Applied::Running
    }

    /// Adopt what the profile change since the last call made stale, in every
    /// window, and repaint only what it asked for.
    ///
    /// Every window is reached because the profile is the *user's* rather than
    /// one window's, and the Settings window follows it for the same reason: a
    /// sheet showing values nothing is using would be as wrong as the pixels
    /// behind it. It presents only what it owes, so a drag in it shows its knob
    /// move and costs nothing more.
    ///
    /// The scoping is what makes a drag smooth. A slider delivers one of these
    /// per motion sample, and the four kinds of work cost wildly different
    /// amounts: a blur change is one bounded message to the compositor and no
    /// pixels at all, whereas a size change re-fits the face, re-derives the
    /// grid, and tells the hosted shell. Doing all of it for whichever one
    /// moved is what made the drag chunky.
    ///
    /// # Errors
    ///
    /// `()` when a present was refused, which ends the process fail-loud: a
    /// window whose pixels cannot be shown is not a window.
    fn apply_profile_change(
        windows: &mut [TerminalWindow],
        client: &mut WindowClient<app::RtWindowTransport>,
        ctx: &mut AppContext<'_>,
    ) -> Result<(), ()> {
        let changed = ctx.publication.take_pending(Invalidation::between);
        let profile = *ctx.publication.live();
        let theme = ctx.themes.active();
        let scale = ctx.desktop.scale();
        for open in windows.iter_mut() {
            if changed.any() {
                open.look.adopt(changed, &profile, theme, ctx.desktop);
            }
            open.restyle(changed, &profile, client).map_err(|_| ())?;
        }
        if let Some(held) = ctx.settings.as_mut() {
            held.follow(&profile, theme, scale);
            present_settings(held, theme, scale, client).map_err(|_| ())?;
        }
        Ok(())
    }

    /// Bring every window up to a changed desktop through the path a profile
    /// change takes, so each repaints only what the change moved: a new scale
    /// or a screen the face no longer fits refits the grid, and a light/dark
    /// switch moves nothing a terminal draws.
    ///
    /// The Settings window is drawn in the desktop's theme, so it is repainted
    /// whole.
    ///
    /// # Errors
    ///
    /// `()` when a present was refused, as for [`apply_profile_change`].
    fn restyle_windows(
        windows: &mut [TerminalWindow],
        client: &mut WindowClient<app::RtWindowTransport>,
        ctx: &mut AppContext<'_>,
    ) -> Result<(), ()> {
        let profile = *ctx.publication.live();
        let theme = ctx.themes.active();
        let scale = ctx.desktop.scale();
        for open in windows.iter_mut() {
            let changed = open.look.refresh(theme, ctx.desktop);
            open.restyle(changed, &profile, client).map_err(|_| ())?;
        }
        if let Some(held) = ctx.settings.as_mut() {
            // New colours and new metrics leave nothing on the sheet
            // standing, and a new scale wants a new extent.
            held.picture.invalidate();
            held.refit(client, settings_extent(ctx.desktop));
            present_settings(held, theme, scale, client).map_err(|_| ())?;
        }
        Ok(())
    }

    /// The process-wide state applying one drained outcome may reach.
    struct AppContext<'a> {
        /// The wait-set every window's members live on.
        set: u64,
        /// The one event mailbox.
        event_endpoint: u64,
        /// The session the desktop query named: every event and every window
        /// reply must come from it.
        server: ProcId,
        /// The next window slot to mint.
        next_slot: &'a mut u64,
        /// The user's profile — what the windows render, and what the store
        /// last said — shared by every window.
        publication: &'a mut Publication<Profile>,
        /// The theme registry the appearance is switched in, and whose
        /// active theme every window draws with.
        themes: &'a mut ThemeRegistry,
        /// The desktop the windows are shown on.
        desktop: &'a mut Desktop,
        /// The worker every settled edit is published through.
        publisher: &'a Publisher,
        /// This terminal's inherited environment.
        env: &'a [Vec<u8>],
        /// The shells of closed windows, still to be reaped.
        ending: &'a mut EndingShells,
        /// The Settings window, while it is open.
        settings: &'a mut Option<SettingsWindow>,
        /// The workers every window's screen effects spread across.
        pool: &'static EffectPool,
    }

    /// Whether the process carries on after an outcome was applied.
    enum Applied {
        /// Keep serving.
        Running,
        /// *Quit* was chosen: end cleanly.
        Ended,
        /// A channel died: end fail-loud with this reason.
        Lost(&'static str),
    }

    /// Apply one drained outcome to the window it names, or to the process,
    /// then bring the screen up to date with whatever it left undrawn.
    fn apply_outcome(
        outcome: EventOutcome,
        windows: &mut Vec<TerminalWindow>,
        client: &mut WindowClient<app::RtWindowTransport>,
        mut ctx: AppContext<'_>,
    ) -> Applied {
        let applied = dispatch_outcome(outcome, windows, client, &mut ctx);
        if !matches!(applied, Applied::Running) {
            return applied;
        }
        // The catch-up: anything an outcome edited and did not draw is
        // drawn here, so the loop paints from the state a wake left behind
        // rather than each handler painting for itself. Costs nothing when
        // the screen is already current, which is the common case.
        if apply_profile_change(windows, client, &mut ctx).is_err() {
            return Applied::Lost("present refused");
        }
        // A window the drain's events changed is painted here, once however
        // many of them changed it.
        for open in windows.iter_mut().filter(|open| open.unpainted) {
            if open.present(client).is_err() {
                return Applied::Lost("present refused");
            }
        }
        applied
    }

    /// Carry out one drained outcome. The catch-up paint is
    /// [`apply_outcome`]'s, so nothing here has to remember it.
    #[allow(clippy::too_many_lines)] // One dispatch over the whole outcome vocabulary; splitting it would hide the ordering.
    #[allow(clippy::needless_pass_by_value)] // The outcome is consumed here: applying it is what ends its life.
    fn dispatch_outcome(
        outcome: EventOutcome,
        windows: &mut Vec<TerminalWindow>,
        client: &mut WindowClient<app::RtWindowTransport>,
        ctx: &mut AppContext<'_>,
    ) -> Applied {
        // A window-scoped outcome names its window; one the list no longer
        // holds is an outcome for a window that has just closed, and there is
        // nothing left to apply it to (fail closed).
        let index = |windows: &[TerminalWindow], window: u64| {
            windows.iter().position(|open| open.pane.id() == window)
        };
        match outcome {
            EventOutcome::Continue => Applied::Running,
            EventOutcome::NewWindow => {
                // No count of its own: every resource a window costs is
                // already bounded by something derived from the machine and
                // enforced with a typed refusal — the session's per-client
                // frame budget, and this process's own stream, process, and
                // address-space limits. `open_window` states the reason on
                // stderr and the terminal carries on, so a second
                // hand-picked ceiling in front of those would only refuse
                // windows the machine could have given.
                let slot = *ctx.next_slot;
                match open_window(WindowContext {
                    client,
                    event_endpoint: ctx.event_endpoint,
                    server: ctx.server,
                    set: ctx.set,
                    slot,
                    profile: ctx.publication.live(),
                    theme: ctx.themes.active(),
                    desktop: ctx.desktop,
                    env: ctx.env,
                    ending: ctx.ending,
                    scrollback: usize::from(ctx.publication.live().scrollback_lines),
                    pool: ctx.pool,
                }) {
                    Ok(opened) => {
                        *ctx.next_slot = slot.saturating_add(1);
                        windows.push(opened);
                    }
                    Err(why) => {
                        let _ = app::refuse(
                            client,
                            (OWN_WORD, ctx.event_endpoint),
                            None,
                            NO_WINDOW,
                            why,
                        );
                    }
                }
                Applied::Running
            }
            EventOutcome::Quit => {
                if let Some(held) = ctx.settings.take() {
                    held.close(client);
                }
                for open in windows.drain(..) {
                    open.hang_up(client, ctx.set, ctx.ending);
                }
                Applied::Ended
            }
            EventOutcome::CloseWindow { window } => {
                let Some(index) = index(windows, window) else {
                    return Applied::Running;
                };
                let owned = close_settings_of(ctx.settings, window, client);
                windows.remove(index).hang_up(client, ctx.set, ctx.ending);
                // The terminal is not its windows: it keeps its icon-bar
                // slot with none open, and a click there opens the next.
                // Only *Quit* ends it. A Settings window that closed with this
                // one settles what it last showed, as its own close does.
                if owned {
                    return dispatch_outcome(
                        EventOutcome::ProfileChanged { settled: true },
                        windows,
                        client,
                        ctx,
                    );
                }
                Applied::Running
            }
            EventOutcome::OpenMenu { window, at } => {
                let Some(index) = index(windows, window) else {
                    return Applied::Running;
                };
                open_window_menu(client, &mut windows[index], at);
                Applied::Running
            }
            EventOutcome::OpenSheet { window } => {
                // The terminal has one Settings window: asked for again, from
                // any window, it comes forward.
                if let Some(held) = ctx.settings.as_ref() {
                    if let Err(err) = client.activate_window(held.pane.id()) {
                        app::report(
                            OWN_WORD,
                            format_args!(
                                "the settings window could not be brought forward ({err})"
                            ),
                        );
                    }
                    return Applied::Running;
                }
                if index(windows, window).is_none() {
                    return Applied::Running;
                }
                *ctx.settings = open_settings(
                    client,
                    (window, ctx.server),
                    ctx.event_endpoint,
                    Box::new(Settings::new(ctx.publication.live())),
                    ctx.themes.active(),
                    ctx.desktop,
                );
                Applied::Running
            }
            EventOutcome::SettingsChanged => {
                // Only the sheet's own pixels moved, so no terminal window is
                // touched.
                let scale = ctx.desktop.scale();
                if let Some(held) = ctx.settings.as_mut() {
                    if present_settings(held, ctx.themes.active(), scale, client).is_err() {
                        return Applied::Lost("settings present refused");
                    }
                }
                Applied::Running
            }
            EventOutcome::SettingsResized {
                width_px,
                height_px,
            } => {
                let scale = ctx.desktop.scale();
                if let Some(held) = ctx.settings.as_mut() {
                    held.refit(client, (width_px, height_px));
                    if present_settings(held, ctx.themes.active(), scale, client).is_err() {
                        return Applied::Lost("settings present refused");
                    }
                }
                Applied::Running
            }
            EventOutcome::ViewMoved { window } => {
                let Some(index) = index(windows, window) else {
                    return Applied::Running;
                };
                if windows[index].present(client).is_err() {
                    return Applied::Lost("present refused");
                }
                Applied::Running
            }
            EventOutcome::Repaint { window } => {
                let Some(index) = index(windows, window) else {
                    return Applied::Running;
                };
                // The session dropped this window's pixels (a redraw
                // request) or the grid was blanked, so the retained picture
                // cannot be trusted.
                windows[index].screen.invalidate();
                if windows[index].present(client).is_err() {
                    return Applied::Lost("present refused");
                }
                Applied::Running
            }
            EventOutcome::ProfileChanged { settled } => {
                // The user changed a setting, so every window adopts what that
                // setting made stale: the profile is the *user's* rather than
                // one window's, but only the part that moved is re-derived.
                //
                // The change is published *only* once the interaction that made
                // it has settled — a slider still under the pointer is one
                // sample of a drag, and writing the store per sample is the
                // freeze this arrangement removes — and the write itself always
                // happens on the publisher's worker, so no gesture waits for a
                // store either way.
                let job = if settled {
                    ctx.publication.settle()
                } else {
                    None
                };
                let ready = job.is_some_and(|job| ctx.publisher.submit(job));
                if apply_profile_change(windows, client, &mut *ctx).is_err() {
                    return Applied::Lost("present refused");
                }
                if ready {
                    return adopt_published(windows, client, &mut *ctx);
                }
                Applied::Running
            }
            EventOutcome::ProfileRestored => {
                // *Restore defaults* removes the user's opinions and adopts
                // what the layers beneath them then imply — which only the
                // store knows, so nothing changes on screen until it answers.
                let job = ctx.publication.restore();
                if job.is_some_and(|job| ctx.publisher.submit(job)) {
                    return adopt_published(windows, client, &mut *ctx);
                }
                Applied::Running
            }
            EventOutcome::ProfilePublished => {
                // The store answered. What it now holds is what applies
                // wherever the user is not editing, so a machine policy or a
                // shipped default wins over the widget's guess and a refused
                // write reverts — stated on `stderr`, never silently kept.
                adopt_published(windows, client, &mut *ctx)
            }
            EventOutcome::DesktopChanged => {
                // The desktop and the theme registry were both already brought
                // into step where the notice was read.
                if restyle_windows(windows, client, &mut *ctx).is_err() {
                    return Applied::Lost("present refused");
                }
                Applied::Running
            }
            EventOutcome::ChannelLost => Applied::Lost("event mailbox lost"),
        }
    }

    /// What the event-mailbox drain concluded.
    ///
    /// Every window-scoped variant names the window it belongs to, because
    /// one mailbox serves every window this process owns: an outcome that did
    /// not say which window it was for could be applied to the wrong one.
    enum EventOutcome {
        /// Every pending event was applied and nothing on screen changed.
        Continue,
        /// Open another terminal window: the icon-bar slot's primary click,
        /// or its *New window* row.
        NewWindow,
        /// Close every window and end the process: the icon-bar menu's
        /// *Quit* row.
        Quit,
        /// This window is done — the desktop asked, the user chose *Close*,
        /// or its shell's stdin is gone. Its siblings keep running, and so
        /// does the process when it was the last.
        CloseWindow {
            /// The window that is closing.
            window: u64,
        },
        /// This window must be repainted whole: the session dropped its
        /// pixels and asked for them again, or its grid was blanked outright.
        Repaint {
            /// The window to repaint.
            window: u64,
        },
        /// This window's view moved over its scrollback: what changed is
        /// painted and presented, as for shell output.
        ViewMoved {
            /// The window whose view moved.
            window: u64,
        },
        /// Only the Settings window's own pixels changed; re-present it and
        /// leave every terminal window alone.
        SettingsChanged,
        /// The session fitted the Settings window to a new client extent.
        SettingsResized {
            /// The new client width.
            width_px: u32,
            /// The new client height.
            height_px: u32,
        },
        /// A secondary press asked for this window's menu at this
        /// client-local point; the caller asks the desktop to open it there.
        OpenMenu {
            /// The window the press landed on.
            window: u64,
            /// Where the press landed, relative to the client origin.
            at: Point,
        },
        /// The *Settings* command asked for the Settings window from this
        /// window: the caller opens it as a dialog of this window, or brings
        /// the one already open forward.
        OpenSheet {
            /// The window the command was chosen in.
            window: u64,
        },
        /// The user changed a setting: adopt what it made stale in every
        /// window. The profile is the user's, so no window owns the change.
        ProfileChanged {
            /// Whether the interaction that made the change has finished, and
            /// so whether the profile is asked to be written. A slider still
            /// under the pointer sets this `false`: the change is live, and a
            /// store write per pointer sample is what this avoids.
            settled: bool,
        },
        /// The user asked for *Restore defaults*: remove their own opinions
        /// from the store and adopt whatever the layers beneath imply.
        ProfileRestored,
        /// The settings worker answered: adopt the profile the store now
        /// holds, or state why it refused the write.
        ProfilePublished,
        /// The desktop changed (screen size, scale, or appearance); already
        /// adopted, along with the theme registry, before this is raised. It
        /// is a property of the seat, so it reaches every window.
        DesktopChanged,
        /// The mailbox itself failed: end fail-loud.
        ChannelLost,
    }

    /// Carry out `command` for the window it was chosen in, reporting what
    /// the caller must now do.
    fn run_command<S: ShellSource>(
        command: Command,
        window: u64,
        terminal: &mut Terminal<S>,
        publication: &mut Publication<Profile>,
    ) -> EventOutcome {
        // A menu row is one whole interaction, so each of these settles.
        let mut resize = |change: fn(&mut Profile)| {
            let was = *publication.live();
            let mut now = was;
            change(&mut now);
            publication.edit(&was, &now);
            EventOutcome::ProfileChanged { settled: true }
        };
        match command {
            Command::Settings => EventOutcome::OpenSheet { window },
            Command::Larger => resize(Profile::enlarge),
            Command::Smaller => resize(Profile::reduce),
            Command::ActualSize => resize(|profile| {
                profile.font_size_px = Profile::default().font_size_px;
            }),
            Command::Clear => {
                terminal.clear();
                EventOutcome::Repaint { window }
            }
            Command::ClearScrollback => {
                terminal.clear_scrollback();
                EventOutcome::ViewMoved { window }
            }
            Command::Close => EventOutcome::CloseWindow { window },
        }
    }

    /// Route the pointer input one event delivered for the Settings window
    /// carries — a move and a press or release, or a wheel turn — into its
    /// sheet, hit-tested against that window's own client.
    fn route_settings_pointer(
        window: &mut SettingsWindow,
        publication: &mut Publication<Profile>,
        input: impl IntoIterator<Item = InputEvent>,
        scale: Scale,
        theme: &Theme,
    ) -> SettingsRouting {
        let viewport = window.viewport();
        let mut routing = SettingsRouting::Nothing;
        let SettingsWindow { sheet, picture, .. } = window;
        let damage = picture.sink();
        for event in input {
            let was = *sheet.profile();
            let outcome = sheet.on_pointer(&event, viewport, scale, theme, damage);
            // Every edit shows at once; only a settled one asks to be written.
            // A drag delivers many samples per gesture, so `Edited` is what
            // keeps the store out of the pointer's path.
            publication.edit(&was, sheet.profile());
            match outcome {
                SheetOutcome::Ignored => {}
                SheetOutcome::Changed => routing = SettingsRouting::Redraw,
                SheetOutcome::Edited => routing = SettingsRouting::Edited,
                SheetOutcome::Settled => routing = SettingsRouting::Settled,
                SheetOutcome::Restore => return SettingsRouting::Restore,
                SheetOutcome::Dismissed => return SettingsRouting::Closed,
            }
        }
        routing
    }

    /// Route one key press delivered for the Settings window into its sheet.
    fn route_settings_key(
        window: &mut SettingsWindow,
        publication: &mut Publication<Profile>,
        key: tairix_abi::input::KeyInput,
        scale: Scale,
        theme: &Theme,
    ) -> SettingsRouting {
        let InputEvent::KeyPressed { key, modifiers } = key_input_event(key) else {
            return SettingsRouting::Nothing;
        };
        let viewport = window.viewport();
        let SettingsWindow { sheet, picture, .. } = window;
        let damage = picture.sink();
        let was = *sheet.profile();
        let outcome = sheet.on_key(key, modifiers, viewport, scale, theme, damage);
        publication.edit(&was, sheet.profile());
        match outcome {
            SheetOutcome::Ignored => SettingsRouting::Nothing,
            SheetOutcome::Changed => SettingsRouting::Redraw,
            SheetOutcome::Edited => SettingsRouting::Edited,
            SheetOutcome::Settled => SettingsRouting::Settled,
            SheetOutcome::Restore => SettingsRouting::Restore,
            SheetOutcome::Dismissed => SettingsRouting::Closed,
        }
    }

    /// What the drain does with what routing an event into `held`
    /// concluded: an outcome to end the drain on, or `None` to read on with
    /// the redraw or live edit it owes folded in.
    ///
    /// Closing settles whatever the sheet was last showing, so an edit the
    /// user made and then closed the window on is still written.
    fn settings_concluded(
        routing: SettingsRouting,
        held: &mut SettingsWindow,
        redrawn: &mut bool,
        edited: &mut bool,
    ) -> Option<EventOutcome> {
        match routing {
            SettingsRouting::Nothing => None,
            SettingsRouting::Redraw => {
                *redrawn = true;
                None
            }
            SettingsRouting::Edited => {
                *edited = true;
                None
            }
            SettingsRouting::Settled => Some(EventOutcome::ProfileChanged { settled: true }),
            SettingsRouting::Restore => Some(EventOutcome::ProfileRestored),
            SettingsRouting::Closed => {
                held.dismissed = true;
                Some(EventOutcome::ProfileChanged { settled: true })
            }
        }
    }

    /// What routing an event into the Settings window concluded.
    #[derive(Copy, Clone)]
    enum SettingsRouting {
        /// Nothing to do.
        Nothing,
        /// The sheet's own pixels changed; re-present its window.
        Redraw,
        /// The sheet edited the profile while the interaction continues: show
        /// it, write nothing.
        Edited,
        /// The sheet edited the profile and the interaction has finished: show
        /// it and ask for it to be written.
        Settled,
        /// The sheet asked for *Restore defaults*: the user's own opinions
        /// are to be removed and the profile the remaining store layers
        /// imply read back.
        Restore,
        /// The sheet asked to close, having possibly edited the profile.
        Closed,
    }

    /// Drain every queued window event (non-blocking — the wait-set wake
    /// said at least one is pending).
    ///
    /// One mailbox serves the whole process, so each event is demuxed on the
    /// window id it carries — a terminal window, or the Settings window — and
    /// an event that carries **no** window id is
    /// application-scoped: an icon-bar click or menu outcome, which names the
    /// emulator rather than any one of its windows.
    ///
    /// A short frame or a sender other than the desktop session is dropped,
    /// never applied: the mailbox is open to any capable sender, so the
    /// kernel-attested origin is the authentication. A malformed frame from
    /// the authenticated session is likewise refused (never guessed at).
    ///
    /// Reading through the shared stream rather than the mailbox is what
    /// folds a resize-grab's run of client extents to the newest. Applying
    /// each queued sample instead would walk the window back through the
    /// sizes the drag already passed, once the button came up.
    #[allow(clippy::too_many_lines)] // One dispatch over the whole window vocabulary; splitting it would hide the routing order.
    fn drain_events(
        (windows, settings): (&mut [TerminalWindow], &mut Option<SettingsWindow>),
        publication: &mut Publication<Profile>,
        desktop: &Desktop,
        theme: &Theme,
        events: &mut WindowEvents<EventMailbox>,
        client: &mut WindowClient<app::RtWindowTransport>,
    ) -> EventOutcome {
        let scale = desktop.scale();
        let mut redrawn = false;
        // A drag delivers many samples and one paint is worth all of them, so
        // an unsettled edit folds here and the whole drain concludes in one
        // outcome; `Publication` measures what that owes the screen, so
        // nothing is lost by folding. A *settle* still concludes the drain at
        // once: it is one event per gesture, and it owes a write.
        let mut edited = false;
        loop {
            let event = match events.try_wait(client) {
                Ok(Some(event)) => event,
                Ok(None) => return finish(EventOutcome::Continue, redrawn, edited),
                // A frame the session sent that this build cannot decode is
                // dropped and the drain reads on past it; only the mailbox
                // itself failing ends the channel.
                Err(EventError::Undecodable(_)) => continue,
                Err(EventError::Refused(refused)) => {
                    app::report(OWN_WORD, refused);
                    continue;
                }
                Err(EventError::Mailbox(_)) => return EventOutcome::ChannelLost,
            };
            // An application-scoped event names the emulator rather
            // than a window: the icon bar's own click and menu
            // outcomes. The row id is this terminal's own, so an id
            // it never declared names no command and is dropped.
            match event {
                WindowEvent::AppBarDefault => return EventOutcome::NewWindow,
                WindowEvent::AppBarMenu { item, .. } => {
                    return match bar_command(item) {
                        Some(BarCommand::NewWindow) => EventOutcome::NewWindow,
                        Some(BarCommand::Quit) => EventOutcome::Quit,
                        None => EventOutcome::Continue,
                    };
                }
                _ => {}
            }
            // Everything else is window-scoped: resolve which of
            // this process's windows it belongs to. An id none names is a
            // window that has just closed, and the event has nowhere to land.
            let Some(window) = event.window_id() else {
                continue;
            };
            if let Some(held) = settings.as_mut().filter(|held| held.pane.id() == window) {
                let routed = settings_event(
                    held,
                    event,
                    publication,
                    (scale, theme),
                    (&mut redrawn, &mut edited),
                );
                if let Some(outcome) = routed {
                    return outcome;
                }
                continue;
            }
            let Some(index) = windows.iter().position(|open| open.pane.id() == window) else {
                continue;
            };
            let open = &mut windows[index];
            match event {
                WindowEvent::Key { key, .. } => match route_key(&mut open.terminal, key) {
                    KeyRouting::Nothing => {}
                    KeyRouting::ViewMoved => open.unpainted = true,
                    KeyRouting::Command(command) => {
                        return finish(
                            run_command(command, window, &mut open.terminal, publication),
                            redrawn,
                            edited,
                        )
                    }
                    KeyRouting::ShellGone => return EventOutcome::CloseWindow { window },
                },
                // The one answer the desktop owes an open. An id
                // that names anything else answers a gesture already
                // settled, so acting on it would run a stale command.
                WindowEvent::MenuClosed {
                    open_id, outcome, ..
                } if open.menu == Some(open_id) => {
                    open.menu = None;
                    match outcome {
                        MenuOutcome::Chosen(item) => {
                            // A row id this terminal never declared
                            // names no command and is dropped.
                            if let Some(command) = Command::from_item(item) {
                                return finish(
                                    run_command(command, window, &mut open.terminal, publication),
                                    redrawn,
                                    edited,
                                );
                            }
                        }
                        // This menu declares no quick-entry field, so a
                        // committed one answers a row it never asked for and
                        // is dropped rather than guessed at.
                        MenuOutcome::Entered(_) | MenuOutcome::Dismissed => {}
                        MenuOutcome::Refused(reason) => {
                            let why = reason.describe();
                            app::report(OWN_WORD, format_args!("no menu was shown: {why}"));
                        }
                    }
                }
                WindowEvent::Pointer { x, y, action, .. } => {
                    // A secondary press asks the desktop for this window's
                    // menu at that point; the screen is the shell's, so every
                    // other pointer event is a no-op.
                    if action
                        == PointerAction::Pressed(tairix_abi::input::PointerButtonCode::Secondary)
                    {
                        return EventOutcome::OpenMenu {
                            window,
                            at: pointer_point(x, y),
                        };
                    }
                }
                WindowEvent::CloseRequested { .. } => return EventOutcome::CloseWindow { window },
                // The wheel moves the view over the scrollback; the emulator
                // reports no mouse input to the program. A burst of turns is
                // painted once, when the drain is applied.
                WindowEvent::Scrolled { dy, .. } => {
                    if open.terminal.scroll_wheel(dy) {
                        open.unpainted = true;
                    }
                }
                // The session dropped the window's pixels under memory
                // pressure.
                WindowEvent::RedrawRequested { .. } => return EventOutcome::Repaint { window },
                // Nobody can see this window, so the session gave its
                // copy of the pixels back and unmapped the region. Let
                // go of this side too — the pages go only when both do
                // — and paint nothing: the redraw request that follows
                // the window being shown again re-attaches a fresh
                // region and fills it.
                WindowEvent::ContentReleased { .. } => open.pane.release_frames(),
                // The window manager resized the window (a live drag-resize
                // sample, or a maximize/restore). A run of samples queued
                // together has already folded to its newest in the shared
                // reader.
                WindowEvent::Resized {
                    width_px,
                    height_px,
                    ..
                } => open.refit(client, (width_px, height_px)),
                // Focus changes repaint nothing; the screen is
                // shell-driven. The terminal never requests a pick, so
                // a pick conclusion is a session bug and is ignored (an
                // unredeemed delegation is reclaimed by the kernel at
                // exit).
                //
                // Minimized needs no action (the window is hidden and
                // still reachable through the bar's hover picker; the
                // screen is redrawn from the shell on demand). A
                // desktop change was already adopted above (or, on
                // refusal, stated and left the last good state
                // standing); either way there is nothing further to do
                // with the event itself. An icon-bar event was handled
                // before the window demux. These are honest no-ops,
                // not deferred work.
                //
                // A secondary press on Close asks to leave what the
                // window is showing; the terminal has nothing to leave
                // but the window itself, and a primary press already
                // closes it.
                //
                // A `MenuClosed` reaching here failed the guard above
                // — it names no open this window is waiting on — and a
                // stale answer is dropped rather than acted on. The
                // terminal's menu declares no panel row, so no chain
                // of its own ever asks it for a surface.
                // An open target names a document, and a terminal window has
                // none: it hosts a shell, which the user opens things from
                // itself. The desktop never queues one for it — a terminal
                // declares no file associations — and one that arrived would
                // name nothing this window could act on.
                WindowEvent::AlternateCloseRequested { .. }
                | WindowEvent::AppBarDefault
                | WindowEvent::AppBarMenu { .. }
                | WindowEvent::AppBarEntered { .. }
                | WindowEvent::MenuClosed { .. }
                // The layer-surface feeds and a tool window's moves address
                // surfaces this application never opens, so none can arrive
                // here.
                | WindowEvent::TerrainChanged { .. }
                | WindowEvent::AlertClosed { .. }
                | WindowEvent::Authorised { .. }
                | WindowEvent::LayerPointer { .. }
                | WindowEvent::ToolMoved { .. }
                | WindowEvent::Focus { .. }
                | WindowEvent::Minimized { .. }
                | WindowEvent::OpenRequested
                | WindowEvent::FilePicked { .. }
                | WindowEvent::PickCancelled { .. }
                | WindowEvent::FolderPicked { .. }
                | WindowEvent::DragOver { .. }
                | WindowEvent::DragEnded { .. }
                | WindowEvent::SaveOffered { .. }
                | WindowEvent::SaveOfferEnded { .. }
                | WindowEvent::ItemsOffered { .. }
                | WindowEvent::ItemsOfferEnded { .. }
                | WindowEvent::PreviewRendered { .. }
                | WindowEvent::Pinch { .. } => {}
            }
        }
    }

    /// The command an icon-bar menu row names, or `None` for an id this
    /// terminal never declared.
    ///
    /// The mapping is the declaration's own
    /// ([`tairix_terminal::appbar`]), so the rows the bar draws and the
    /// commands they run are stated once.
    fn bar_command(item: AppMenuItemId) -> Option<BarCommand> {
        BarCommand::from_item(item)
    }

    /// Fold a pending Settings redraw into `outcome`: a command that
    /// concluded nothing still re-presents the Settings window when an
    /// earlier event in the same drain changed its pixels.
    fn finish(outcome: EventOutcome, redrawn: bool, edited: bool) -> EventOutcome {
        match (outcome, edited, redrawn) {
            // A profile edit outranks a bare redraw: applying it re-presents
            // the sheet anyway, so the redraw is not also owed.
            (EventOutcome::Continue, true, _) => EventOutcome::ProfileChanged { settled: false },
            (EventOutcome::Continue, false, true) => EventOutcome::SettingsChanged,
            (other, _, _) => other,
        }
    }

    /// Apply one event delivered for the Settings window: an outcome to end
    /// the drain on, or `None` to read on with what it owes folded in.
    fn settings_event(
        held: &mut SettingsWindow,
        event: WindowEvent,
        publication: &mut Publication<Profile>,
        (scale, theme): (Scale, &Theme),
        (redrawn, edited): (&mut bool, &mut bool),
    ) -> Option<EventOutcome> {
        let routing = match event {
            WindowEvent::Key { key, .. } => {
                route_settings_key(held, publication, key, scale, theme)
            }
            WindowEvent::Pointer { x, y, action, .. } => {
                let input = pointer_input_events(action, pointer_point(x, y));
                route_settings_pointer(held, publication, input, scale, theme)
            }
            // The wheel over the sheet scrolls its body.
            WindowEvent::Scrolled { x, y, dx, dy, .. } => {
                let input = scroll_input_events(pointer_point(x, y), dx, dy);
                route_settings_pointer(held, publication, input, scale, theme)
            }
            // Its close control is the *Done* button's.
            WindowEvent::CloseRequested { .. } => SettingsRouting::Closed,
            WindowEvent::RedrawRequested { .. } => {
                held.picture.invalidate();
                SettingsRouting::Redraw
            }
            // Hidden with its owner: the region goes back as a terminal
            // window's does, and the redraw that follows its showing again
            // fills a fresh one.
            WindowEvent::ContentReleased { .. } => {
                held.pane.release_frames();
                SettingsRouting::Nothing
            }
            WindowEvent::Resized {
                width_px,
                height_px,
                ..
            } => {
                return Some(EventOutcome::SettingsResized {
                    width_px,
                    height_px,
                })
            }
            // The keyboard coming or going, a minimise with its owner, and
            // a secondary press on Close change nothing it shows; the rest
            // address surfaces and requests the sheet never makes.
            WindowEvent::Focus { .. }
            | WindowEvent::Minimized { .. }
            | WindowEvent::AlternateCloseRequested { .. }
            | WindowEvent::AppBarDefault
            | WindowEvent::AppBarMenu { .. }
            | WindowEvent::AppBarEntered { .. }
            | WindowEvent::MenuClosed { .. }
            | WindowEvent::TerrainChanged { .. }
            | WindowEvent::AlertClosed { .. }
            | WindowEvent::Authorised { .. }
            | WindowEvent::LayerPointer { .. }
            | WindowEvent::ToolMoved { .. }
            | WindowEvent::OpenRequested
            | WindowEvent::FilePicked { .. }
            | WindowEvent::PickCancelled { .. }
            | WindowEvent::FolderPicked { .. }
            | WindowEvent::DragOver { .. }
            | WindowEvent::DragEnded { .. }
            | WindowEvent::SaveOffered { .. }
            | WindowEvent::SaveOfferEnded { .. }
            | WindowEvent::ItemsOffered { .. }
            | WindowEvent::ItemsOfferEnded { .. }
            | WindowEvent::PreviewRendered { .. }
            | WindowEvent::Pinch { .. } => SettingsRouting::Nothing,
        };
        settings_concluded(routing, held, redrawn, edited)
    }

    /// What routing a key press delivered for the terminal's own window
    /// concluded.
    enum KeyRouting {
        /// Nothing to do.
        Nothing,
        /// A terminal accelerator named this command.
        Command(Command),
        /// The view moved over the scrollback.
        ViewMoved,
        /// The shell can no longer accept input.
        ShellGone,
    }

    /// Route one key press delivered for the terminal's own window: a
    /// terminal accelerator claims it, then a scrolling chord, else it is
    /// shell input, which brings the view back to the live screen first.
    ///
    /// A key for the Settings window never reaches here — it arrives under
    /// that window's own id and is routed there.
    fn route_key<S: ShellSource>(
        terminal: &mut Terminal<S>,
        key: tairix_abi::input::KeyInput,
    ) -> KeyRouting {
        let input = key_input_event(key);
        if let InputEvent::KeyPressed { key, modifiers } = input {
            if let Some(command) = Command::accelerator(key, modifiers) {
                return KeyRouting::Command(command);
            }
            if let Some(moved) = terminal.view_key(key, modifiers) {
                return if moved {
                    KeyRouting::ViewMoved
                } else {
                    KeyRouting::Nothing
                };
            }
        }
        // The one shared layout-to-tty rule; a release encodes zero bytes
        // and sends nothing.
        let mut bytes = [0u8; MAX_KEY_BYTES];
        let Ok(n) = encode_key_input(&key, &mut bytes) else {
            return KeyRouting::Nothing;
        };
        match bytes.get(..n) {
            Some(slice) if n > 0 => {
                let moved = terminal.view_to_bottom();
                if terminal.send(slice).is_err() {
                    return KeyRouting::ShellGone;
                }
                if moved {
                    KeyRouting::ViewMoved
                } else {
                    KeyRouting::Nothing
                }
            }
            _ => KeyRouting::Nothing,
        }
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

//! The `patience.app` bundle's `Run` entry point: Klondike, FreeCell and
//! Spider, with a solver behind every hint.
//!
//! Everything with behaviour lives in the host-tested model
//! (`tairix_patience`); this binary composes it over the live window channel:
//!
//! * a park carrying the game's own one-shot deadline — the next frame of
//!   anything moving, the clock's next second, a note's end, the kept game's
//!   write — and none at all while the table is still and no clock runs, so a
//!   game left alone costs no wake;
//! * each wake draining every event already delivered before one frame is
//!   painted for all of them;
//! * three workers, because none of their work may run on the loop that owes
//!   the window a frame: the store's writer, the analyst answering what the
//!   solver finds about the position in play, and the dealer finding deals the
//!   solver can win;
//! * every write the game owes seen out before the program ends.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy, and
//! fmt still cover the file.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
extern crate alloc;

#[cfg(freestanding)]
mod program {
    use core::cell::Cell;

    use alloc::sync::Arc;
    use alloc::vec::Vec;

    use tairix_abi::driver::display::{DamageRect, DisplayMode};
    use tairix_abi::input::KeyInput;
    use tairix_abi::latency::DEFAULT_FRAME_BUDGET_NS;
    use tairix_abi::window_ipc::{
        AlertOutcome, AppBar, AppBarClick, AppMenu, AppMenuItemId, MenuOutcome, WindowEvent,
        WindowRegion,
    };
    use tairix_abi::{Errno, ProcId};
    use tairix_appdata::{Refusal, RtHost, Settings};
    use tairix_font::BitmapFont;
    use tairix_geometry::{Point, Rect, Region};
    use tairix_input::InputEvent;
    use tairix_patience::analysis::{self, DealFound, DealSearch, Finding, Question};
    use tairix_patience::game::{Alerting, Game, Setup};
    use tairix_patience::layout::WindowGeometry;
    use tairix_patience::menu::{Command, DEAL_ENTRY};
    use tairix_patience::prefs::Preferences;
    use tairix_patience::saved::Saved;
    use tairix_patience::solver::Solver;
    use tairix_patience::stats::Stats;
    use tairix_patience::store::{self, Answer, Job};
    use tairix_rng::FastRng;
    use tairix_rt::sync::WorkerWake;
    use tairix_rt::work::{Worker, WorkerGuard};
    use tairix_theme::{TextRole, ThemeRegistry};
    use tairix_window::app::{self, AppWindow, Wake, EXIT_CHANNEL_LOST};
    use tairix_window::{
        key_input_event, pointer_input_events, pointer_point, present_damage, Desktop, EventDrain,
        EventError, EventMailbox, EventSource, Parked, Repaint, WindowEvents,
    };

    /// The wait-set tokens the workers' answer wakes arrive under.
    const WRITER_TOKEN: u64 = app::FIRST_APP_TOKEN;
    const ANALYST_TOKEN: u64 = app::FIRST_APP_TOKEN + 1;
    const DEALER_TOKEN: u64 = app::FIRST_APP_TOKEN + 2;

    /// The name this program states its refusals under.
    const APP_NAME: &str = "patience";
    /// The window's title.
    const TITLE: &str = "Patience";

    /// The store's writer. Latest-wins, but the game's ledger submits a write
    /// only once the one before it has landed, so none is ever displaced.
    type Writer = Worker<(), Job, Answer>;
    /// The analyst: a question asked meanwhile replaces one not yet begun,
    /// because only the position in play is worth an answer.
    type Analyst = Worker<Solver, Question, Finding>;
    /// The dealer: a search asked meanwhile replaces one not yet begun.
    type Dealer = Worker<Solver, DealSearch, DealFound>;

    fn write(_: &mut (), job: &mut Job) -> Answer {
        store::carry_out(job, &mut Settings::open_without_defaults(&mut RtHost))
    }

    /// The three workers.
    struct Workers {
        writer: Arc<Writer>,
        analyst: Arc<Analyst>,
        dealer: Arc<Dealer>,
    }

    impl Workers {
        /// Each worker, started on its own thread where the machine grants
        /// one; one it does not runs its work on this thread instead.
        fn start(set: u64) -> Self {
            let workers = Self {
                writer: Arc::new(Worker::new(write, (), WorkerWake::create())),
                analyst: Arc::new(Worker::new(
                    analysis::study,
                    Solver::new(),
                    WorkerWake::create(),
                )),
                dealer: Arc::new(Worker::new(
                    analysis::find_deal,
                    Solver::new(),
                    WorkerWake::create(),
                )),
            };
            app::start_worker(APP_NAME, &workers.writer, set, WRITER_TOKEN, "store");
            app::start_worker(APP_NAME, &workers.analyst, set, ANALYST_TOKEN, "analysis");
            app::start_worker(APP_NAME, &workers.dealer, set, DEALER_TOKEN, "dealing");
            workers
        }
    }

    // ---- the event source ----------------------------------------------

    /// The app's park: its event mailbox, the memory-pressure band, the
    /// workers' answer wakes, and the game's own deadline.
    struct RtEventSource<'a> {
        mailbox: EventMailbox,
        set: u64,
        workers: &'a Workers,
        /// When the game next owes a wake, or `None` when it owes none, in
        /// which case the park carries no deadline and the CPU is given up
        /// entirely. The loop writes it just before the park reads it, on the
        /// one thread both run on.
        deadline_ns: &'a Cell<Option<u64>>,
        /// Set when the park woke for a desktop change.
        desktop_moved: &'a Cell<bool>,
        /// Set when the park woke for a new memory-pressure band.
        pressure_moved: &'a Cell<bool>,
    }

    impl EventDrain for RtEventSource<'_> {
        fn try_next(&mut self, event: &mut [u8; WindowEvent::WIRE_LEN]) -> Result<bool, Errno> {
            self.mailbox.try_next(event)
        }
    }

    impl EventSource for RtEventSource<'_> {
        fn park(&mut self) -> Result<Parked, Errno> {
            let woken = match self.deadline_ns.get() {
                Some(deadline) => match app::park_until(self.set, deadline)? {
                    Some(woken) => woken,
                    None => return Ok(Parked::Interrupted),
                },
                None => app::park(self.set)?,
            };
            // A worker's readiness is a level peek, so a wake left undrained
            // would report ready for ever and turn the park into a spin.
            match woken {
                Wake::App(WRITER_TOKEN) => self.workers.writer.wake().drain(),
                Wake::App(ANALYST_TOKEN) => self.workers.analyst.wake().drain(),
                Wake::App(DEALER_TOKEN) => self.workers.dealer.wake().drain(),
                Wake::PressureChanged => {
                    tairix_font::trim_glyph_cache();
                    self.pressure_moved.set(true);
                }
                Wake::DesktopChanged => self.desktop_moved.set(true),
                Wake::Event | Wake::PressureUnchanged | Wake::App(_) => return Ok(Parked::Served),
            }
            Ok(Parked::Interrupted)
        }
    }

    // ---- the shell around the game --------------------------------------

    /// Everything the loop holds besides the game: the window, the desktop
    /// it is drawn for, the workers, and the menus and alerts in flight.
    struct Shell<'a> {
        window: AppWindow,
        endpoint: u64,
        desktop: Desktop,
        themes: ThemeRegistry,
        workers: &'a Workers,
        rng: FastRng,
        /// The icon bar's menu as last declared, so it is declared again only
        /// when what it says has changed.
        declared: Option<AppMenu>,
        /// The table's own menu, while it is open.
        context: Option<u64>,
        /// The alerts up, and what each asks.
        alerts: Vec<(u64, Alerting)>,
        /// Set when an answer was carried out on this thread and is waiting
        /// to be taken, so the next round takes it before it parks.
        landed: bool,
    }

    /// What one round of events concluded for the loop.
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
    struct Concluded {
        /// Every pixel may have moved.
        whole: bool,
        /// End the program.
        quit: bool,
    }

    impl Shell<'_> {
        /// Open a window at the size the game asks for and present its first
        /// frame, answering the session's [`ProcId`] or the reserved exit
        /// code.
        fn open(&mut self, game: &mut Game, now_ns: u64) -> Result<ProcId, i32> {
            let geometry = WindowGeometry::resolve(self.desktop.scale(), self.desktop.screen());
            let mode = app::mode_for(geometry.width, geometry.height);
            let server = self
                .window
                .open(self.endpoint, &mode, TITLE, geometry.sizing)
                .map_err(|err| app::fail(APP_NAME, err.code(), err))?;
            self.adopt(game, &mut tairix_controls::damage::sink());
            game.set_visible(true, now_ns);
            if self.present(game, DamageRect::full(&mode), now_ns).is_err() {
                self.close(game, now_ns);
                return Err(app::fail(APP_NAME, EXIT_CHANNEL_LOST, "present refused"));
            }
            Ok(server)
        }

        fn close(&mut self, game: &mut Game, now_ns: u64) {
            game.set_visible(false, now_ns);
            self.context = None;
            let _ = self.window.close();
        }

        fn mode(&self) -> Option<DisplayMode> {
            self.window.mode().copied()
        }

        /// Lay the game out in the extent the window actually holds.
        fn adopt(&self, game: &mut Game, damage: &mut Region) {
            if let Some(mode) = self.mode() {
                let client = Rect::new(0, 0, mode.width_px, mode.height_px);
                game.relayout(client, self.desktop.scale(), damage);
            }
        }

        /// Adopt the extent the window manager gave the window: re-map the
        /// frame region onto it and lay the game out in it.
        fn resized_to(&mut self, game: &mut Game, mode: DisplayMode, damage: &mut Region) {
            if self.mode().is_none() {
                return;
            }
            if self.window.resize(mode) == app::Fitted::Kept {
                app::report(APP_NAME, app::SIZE_KEPT);
            }
            self.adopt(game, damage);
        }

        /// Adopt the desktop state the session published: the theme, the
        /// density and the reduced-motion policy. The window keeps the size
        /// the user gave it; only the range it may be dragged within, which is
        /// logical, is restated.
        fn adopt_desktop(&mut self, game: &mut Game, damage: &mut Region) -> bool {
            let adopted = app::adopt_desktop(&mut self.desktop, &mut self.themes);
            // A new frame rate alone owes no redraw, but still paces the game.
            game.set_refresh(self.desktop.refresh());
            match adopted {
                Ok(true) => {
                    let reduced = self.themes.active().motion().reduced_motion();
                    game.set_reduced_motion(reduced, damage);
                    self.adopt(game, damage);
                    let geometry =
                        WindowGeometry::resolve(self.desktop.scale(), self.desktop.screen());
                    if let Err(err) = self.window.set_sizing(geometry.sizing) {
                        app::report(
                            APP_NAME,
                            format_args!(
                                "the desktop refused this window's resize range ({err}); \
                                 it keeps the range it had"
                            ),
                        );
                    }
                    true
                }
                Ok(false) => false,
                Err(err) => {
                    app::report(APP_NAME, format_args!("desktop change refused: {err}"));
                    false
                }
            }
        }

        /// Draw the game and present `damage`.
        fn present(
            &mut self,
            game: &mut Game,
            damage: DamageRect,
            now_ns: u64,
        ) -> Result<(), Errno> {
            let theme = self.themes.active();
            let font = BitmapFont::for_role(theme.fonts(), TextRole::Body, self.desktop.scale());
            self.window
                .present(damage, |surface| game.render(surface, theme, font, now_ns))
        }

        /// Declare the icon bar's menu as the game now stands, unless it says
        /// what is already declared.
        fn declare(&mut self, game: &Game) {
            let menu = game.app_menu();
            if self.declared == Some(menu) {
                return;
            }
            let bar = AppBar {
                event_endpoint: self.endpoint,
                click: AppBarClick::RaiseOrOpen,
                menu,
            };
            match tairix_window::declare_app_bar(self.window.client(), Ok(bar)) {
                Ok(()) => self.declared = Some(menu),
                Err(refused) => app::report(APP_NAME, refused),
            }
        }

        /// Apply one delivered event.
        fn apply(
            &mut self,
            game: &mut Game,
            event: &WindowEvent,
            now_ns: u64,
            damage: &mut Region,
        ) -> Concluded {
            let mut concluded = Concluded::default();
            match *event {
                // The game stays on the icon bar, kept, its clock stopped.
                WindowEvent::CloseRequested { .. } => self.close(game, now_ns),
                // A refused open is already stated, and the slot is still
                // there to try again from.
                WindowEvent::AppBarDefault => {
                    if self.mode().is_none() {
                        let _ = self.open(game, now_ns);
                    }
                }
                WindowEvent::AppBarMenu { item } => {
                    if tairix_window::is_quit(item) {
                        concluded.quit = true;
                    } else {
                        self.chosen(game, item, now_ns, damage);
                    }
                }
                WindowEvent::Resized {
                    width_px,
                    height_px,
                    ..
                } => {
                    self.resized_to(game, app::mode_for(width_px, height_px), damage);
                    concluded.whole = true;
                }
                WindowEvent::Key {
                    key: pressed @ KeyInput::Pressed { .. },
                    ..
                } => {
                    game.set_visible(true, now_ns);
                    if let InputEvent::KeyPressed { key, modifiers } = key_input_event(pressed) {
                        let _ = game.on_key(key, modifiers, now_ns, &mut self.rng, damage);
                    }
                }
                WindowEvent::Pointer { x, y, action, .. } => {
                    game.set_visible(true, now_ns);
                    for input in pointer_input_events(action, pointer_point(x, y)) {
                        let _ = game.on_pointer(&input, now_ns, damage);
                    }
                }
                WindowEvent::MenuClosed {
                    window_id,
                    open_id,
                    outcome,
                } if self.context == Some(open_id) => {
                    self.context = None;
                    self.menu_closed(game, (window_id, open_id), outcome, now_ns, damage);
                }
                WindowEvent::AlertClosed { alert_id, outcome } => {
                    if let Some(at) = self.alerts.iter().position(|&(id, _)| id == alert_id) {
                        let (_, alerting) = self.alerts.swap_remove(at);
                        let chosen = match outcome {
                            AlertOutcome::Chosen { index } => Some(index),
                            AlertOutcome::Dismissed => None,
                        };
                        game.alert_answered(alerting, chosen, now_ns, &mut self.rng, damage);
                    }
                }
                WindowEvent::Minimized { .. } => game.set_visible(false, now_ns),
                WindowEvent::Focus { focused: true, .. } => game.set_visible(true, now_ns),
                // A released region holds none of the frame, so the redraw
                // the session asks for afterwards is painted whole.
                WindowEvent::RedrawRequested { .. } => {
                    concluded.whole = self.window.content_released();
                }
                WindowEvent::ContentReleased { .. } => self.window.release_frames(),
                // The rest are events the game does not act on: a secondary
                // press on Close asks to leave what the window shows, and the
                // table is all it shows; it declares no file association, asks
                // for no picker, and opens no surface of another kind, so none
                // of their outcomes can arrive.
                WindowEvent::AlternateCloseRequested { .. }
                | WindowEvent::MenuClosed { .. }
                | WindowEvent::Key { .. }
                | WindowEvent::Focus { .. }
                | WindowEvent::Scrolled { .. }
                | WindowEvent::Pinch { .. }
                | WindowEvent::FilePicked { .. }
                | WindowEvent::PickCancelled { .. }
                | WindowEvent::FolderPicked { .. }
                | WindowEvent::DragOver { .. }
                | WindowEvent::DragEnded { .. }
                | WindowEvent::PreviewRendered { .. }
                | WindowEvent::Authorised { .. }
                | WindowEvent::OpenRequested
                | WindowEvent::TerrainChanged { .. }
                | WindowEvent::LayerPointer { .. }
                | WindowEvent::ToolMoved { .. } => {}
            }
            concluded
        }

        /// Act on a menu row. A row no menu here carries names no command
        /// and is ignored (fail closed).
        fn chosen(
            &mut self,
            game: &mut Game,
            item: AppMenuItemId,
            now_ns: u64,
            damage: &mut Region,
        ) {
            if let Some(command) = Command::of_row(item.get()) {
                let _ = game.command(command, now_ns, &mut self.rng, damage);
            }
        }

        /// Act on how the table's own menu ended.
        fn menu_closed(
            &mut self,
            game: &mut Game,
            (window_id, open_id): (u64, u64),
            outcome: MenuOutcome,
            now_ns: u64,
            damage: &mut Region,
        ) {
            match outcome {
                MenuOutcome::Chosen(item) => self.chosen(game, item, now_ns, damage),
                // The number is wider than the event, so it is pulled.
                MenuOutcome::Entered(item) if item.get() == DEAL_ENTRY => {
                    match self.window.client().take_menu_text(window_id, open_id) {
                        Ok(Some(text)) => {
                            if !game.deal_entered(&text, now_ns, &mut self.rng, damage) {
                                let owner = self.window.window_id();
                                let _ = app::refuse(
                                    self.window.client(),
                                    (APP_NAME, self.endpoint),
                                    owner,
                                    "That is not a deal of this game",
                                    "A deal is numbered from 1, up to 1,000,000 in FreeCell \
                                     and 999,999,999 in the others.",
                                );
                            }
                        }
                        Ok(None) => {}
                        Err(err) => {
                            app::report(APP_NAME, format_args!("typed deal refused ({err})"));
                        }
                    }
                }
                MenuOutcome::Entered(_) | MenuOutcome::Dismissed => {}
                MenuOutcome::Refused(reason) => {
                    app::report(
                        APP_NAME,
                        format_args!("no menu was shown: {}", reason.describe()),
                    );
                }
            }
        }

        /// Take every answer that has landed.
        fn adopt_answers(&mut self, game: &mut Game, now_ns: u64, damage: &mut Region) {
            while let Some(answer) = self.workers.writer.collect() {
                let mut refusals = Vec::new();
                let next = game.store_landed(answer, &mut refusals);
                report_all(refusals);
                if let Some(job) = next {
                    self.landed |= self.workers.writer.submit(job);
                }
            }
            while let Some(finding) = self.workers.analyst.collect() {
                game.answered(finding, now_ns, damage);
            }
            while let Some(found) = self.workers.dealer.collect() {
                game.dealt(found, now_ns, &mut self.rng, damage);
            }
        }

        /// Hand on everything the game asked for this round.
        fn serve_outbox(&mut self, game: &mut Game) {
            if let Some(job) = game.store_job() {
                self.landed |= self.workers.writer.submit(job);
            }
            let outbox = game.outbox();
            let (question, deal) = (outbox.question.take(), outbox.deal.take());
            let (alert, context) = (outbox.alert.take(), outbox.context.take());
            let menu_moved = core::mem::take(&mut outbox.menu_moved);
            if let Some(question) = question {
                self.landed |= self.workers.analyst.submit(question);
            }
            if let Some(search) = deal {
                self.landed |= self.workers.dealer.submit(search);
            }
            if let Some((alerting, spec)) = alert {
                let owner = self.window.window_id();
                match self.window.client().open_alert(self.endpoint, owner, &spec) {
                    Ok(id) => self.alerts.push((id, alerting)),
                    Err(err) => {
                        app::report(
                            APP_NAME,
                            format_args!("the desktop would not show that ({err})"),
                        );
                    }
                }
            }
            if let Some(at) = context {
                self.open_context(game, at);
            }
            if menu_moved {
                self.declare(game);
            }
        }

        /// Open the table's own menu at `at`.
        fn open_context(&mut self, game: &Game, at: Point) {
            let Some(window_id) = self.window.window_id() else {
                return;
            };
            let Ok(anchor) = WindowRegion::new(at.x, at.y, 0, 0) else {
                return;
            };
            match self
                .window
                .client()
                .open_menu(window_id, anchor, &game.context_menu())
            {
                Ok(open_id) => self.context = Some(open_id),
                Err(err) => app::report(APP_NAME, format_args!("no menu was shown ({err})")),
            }
        }
    }

    fn report_all(refusals: Vec<Refusal>) {
        for refusal in refusals {
            app::report(APP_NAME, refusal);
        }
    }

    // ---- the run ---------------------------------------------------------

    /// The event loop: park, drain, apply, paint once. A dead channel ends the
    /// program fail-loud; a quit ends it at zero.
    fn run(
        shell: &mut Shell<'_>,
        game: &mut Game,
        deadline: &Cell<Option<u64>>,
        moved: (&Cell<bool>, &Cell<bool>),
        mut events: WindowEvents<RtEventSource<'_>>,
    ) -> i32 {
        let (desktop_moved, pressure_moved) = moved;
        let mut burst: Vec<WindowEvent> = Vec::new();
        loop {
            deadline.set(game.deadline_ns(tairix_rt::clock_get()));
            let first = if core::mem::take(&mut shell.landed) {
                None
            } else {
                match events.wait(shell.window.client()) {
                    Ok(event) => event,
                    Err(EventError::Undecodable(_)) => None,
                    Err(EventError::Refused(refused)) => {
                        app::report(APP_NAME, refused);
                        None
                    }
                    Err(EventError::Mailbox(_)) => {
                        return app::fail(APP_NAME, EXIT_CHANNEL_LOST, "event channel lost")
                    }
                }
            };
            burst.clear();
            burst.extend(first);
            loop {
                match events.try_wait(shell.window.client()) {
                    Ok(Some(event)) => burst.push(event),
                    Ok(None) | Err(EventError::Undecodable(_)) => break,
                    Err(EventError::Refused(refused)) => app::report(APP_NAME, refused),
                    Err(EventError::Mailbox(_)) => {
                        return app::fail(APP_NAME, EXIT_CHANNEL_LOST, "event channel lost")
                    }
                }
            }

            let now = tairix_rt::clock_get();
            let mut damage = tairix_controls::damage::sink();
            let mut concluded = Concluded::default();
            // Adopted first, so the scale, the theme and the reduced-motion
            // policy everything below draws from are already current.
            if desktop_moved.replace(false) {
                concluded.whole |= shell.adopt_desktop(game, &mut damage);
            }
            if pressure_moved.replace(false) {
                game.trim();
            }
            let _ = game.tick(now, &mut damage);
            shell.adopt_answers(game, now, &mut damage);
            for event in &burst {
                let one = shell.apply(game, event, now, &mut damage);
                concluded.whole |= one.whole;
                concluded.quit |= one.quit;
            }
            if concluded.quit {
                shell.close(game, now);
                return 0;
            }
            shell.serve_outbox(game);

            let Some(mode) = shell.mode() else {
                continue;
            };
            let repaint = if concluded.whole {
                Repaint::Whole
            } else if damage.is_empty() {
                Repaint::Nothing
            } else {
                Repaint::Reported
            };
            let Some(owed) = present_damage(&mode, repaint, &damage) else {
                continue;
            };
            if shell.present(game, owed, now).is_err() {
                return app::fail(APP_NAME, EXIT_CHANNEL_LOST, "present refused");
            }
        }
    }

    /// See every write the game owes out before the program ends: the one in
    /// flight, then the last, tried once more however the one before went.
    fn see_out(game: &mut Game, writer: &Writer) {
        let now = tairix_rt::clock_get();
        let mut next = game.final_job(now);
        let mut final_tried = next.is_some();
        loop {
            if let Some(job) = next.take() {
                let _ = writer.submit(job);
            }
            let Some(answer) = writer.wait() else {
                if final_tried {
                    return;
                }
                final_tried = true;
                next = game.final_job(now);
                if next.is_none() {
                    return;
                }
                continue;
            };
            let mut refusals = Vec::new();
            next = game.store_landed(answer, &mut refusals);
            report_all(refusals);
            if next.is_none() && !final_tried {
                final_tried = true;
                next = game.final_job(now);
            }
        }
    }

    /// What the store held at start, saying what of it could not be read.
    fn load() -> (
        Preferences,
        Stats,
        Option<(tairix_patience::play::Play, u32)>,
    ) {
        let mut host = RtHost;
        let store = Settings::open_without_defaults(&mut host);
        let (prefs, mut refused) = tairix_appdata::loaded::<Preferences>(&store);
        let (stats, more) = tairix_appdata::loaded::<Stats>(&store);
        refused.extend(more);
        let (saved, more) = tairix_appdata::loaded::<Saved>(&store);
        refused.extend(more);
        // The three registries share one store, so a store that cannot be
        // read is said once, not once for each.
        let mut said: Vec<Refusal> = Vec::new();
        for refusal in refused {
            if !said.contains(&refusal) {
                said.push(refusal);
            }
        }
        report_all(said);
        (prefs, stats, saved.resume())
    }

    /// A generator seeded from the kernel's own CSPRNG, so no two shuffles
    /// are predictable from the ones before; seeded from the clock when the
    /// kernel's is not ready, because a game of patience has no
    /// unpredictability requirement worth refusing to start over.
    fn seeded_rng() -> FastRng {
        FastRng::keyed_by(tairix_rt::random_fill).unwrap_or_else(|_| {
            app::report(
                APP_NAME,
                "the system generator is unavailable; seeding this session from the clock",
            );
            FastRng::seed_from_u64(tairix_rt::clock_get())
        })
    }

    /// Program entry point. `tairix-rt`'s `_start` calls it once the runtime is
    /// set up and routes its return value through the `exit` syscall.
    fn main() -> i32 {
        // From here this task drives a user-facing loop, so declare the frame
        // it owes; a shippable image arms nothing.
        let _ = tairix_rt::latency_watch(DEFAULT_FRAME_BUDGET_NS);

        let mut window = AppWindow::new();
        let (desktop, themes) = match app::bring_up_desktop(window.client()) {
            Ok(pair) => pair,
            Err(err) => return app::fail(APP_NAME, err.code(), err),
        };
        let binding = match app::bind_event_mailbox() {
            Ok(binding) => binding,
            Err(err) => return app::fail(APP_NAME, err.code(), err),
        };
        let workers = Workers::start(binding.set());
        let _writer = WorkerGuard::new(&workers.writer);
        let _analyst = WorkerGuard::new(&workers.analyst);
        let _dealer = WorkerGuard::new(&workers.dealer);

        let (prefs, stats, resume) = load();
        let now = tairix_rt::clock_get();
        let opening = WindowGeometry::resolve(desktop.scale(), desktop.screen());
        let mut rng = seeded_rng();
        let setup = Setup {
            prefs,
            stats,
            resume,
            client: opening.client(),
            scale: desktop.scale(),
            reduced: themes.active().motion().reduced_motion(),
            refresh: desktop.refresh(),
        };
        let mut game = Game::new(setup, now, &mut rng);
        let mut shell = Shell {
            window,
            endpoint: binding.endpoint(),
            desktop,
            themes,
            workers: &workers,
            rng,
            declared: None,
            context: None,
            alerts: Vec::new(),
            landed: false,
        };

        // The presence first: declared before this process owns a window, its
        // slot carries this menu from the moment it appears.
        shell.declare(&game);
        // A first window that will not open leaves the game nothing to be, so
        // it ends fail-loud; every later one is a click on its slot.
        let server = match shell.open(&mut game, now) {
            Ok(server) => server,
            Err(code) => return code,
        };
        shell.serve_outbox(&mut game);

        let deadline = Cell::new(None);
        let desktop_moved = Cell::new(false);
        let pressure_moved = Cell::new(false);
        let events = WindowEvents::new(RtEventSource {
            mailbox: EventMailbox::new(shell.endpoint, server),
            set: binding.set(),
            workers: &workers,
            deadline_ns: &deadline,
            desktop_moved: &desktop_moved,
            pressure_moved: &pressure_moved,
        });
        let code = run(
            &mut shell,
            &mut game,
            &deadline,
            (&desktop_moved, &pressure_moved),
            events,
        );
        see_out(&mut game, &workers.writer);
        code
    }

    tairix_rt::entry!(main);
}

// --- Host stub ----------------------------------------------------------
//
// On the host the program's real entry — the freestanding `tairix-rt`
// `_start` path — is not compiled, so this inert `main` keeps the crate
// building under the host tooling. It performs no I/O.
#[cfg(not(freestanding))]
fn main() {}

//! The `Run` entry-point binary of `play`: the program a shell spawns to play
//! sound files.
//!
//! A pure-Rust program over `tairix-rt`. It is also its own decoder: spawned
//! by itself in the sandbox's session-worker role, it serves the decode
//! protocol over the two pipes the sandbox wires and nothing else.
//!
//! # Two loops, neither of which polls
//!
//! Playback runs on the main thread, parked on one wait-set holding the
//! decoder's pipes, the stream's mailbox, the commands the interface sends,
//! and the signal intake; a paced decoder restart is its only timer. The
//! full-screen interface, when there is a terminal to draw it on, is a thread
//! of its own, parked on the keyboard relay, a wake the playback loop nudges
//! when the status changes, and the terminal's foreground edge. It paints from
//! the status alone, at most once a frame, and only while this process holds
//! the terminal — so `play` keeps playing in the background and draws itself
//! again when it is brought back.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy, and
//! fmt still cover the file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

#[cfg(all(freestanding, feature = "program"))]
mod program {
    extern crate alloc;

    use alloc::format;
    use alloc::string::String;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::fmt::Write as _;

    use tairix_abi::audio::{AudioDeviceDescriptor, AudioGain};
    use tairix_abi::driver::audio::StreamDirection;
    use tairix_abi::waitset::{WaitSetOp, WaitSourceKind};
    use tairix_abi::{
        Errno, InputMode, Origin, Signal, SignalIntakeOp, ORIGIN_WIRE_LEN, STDERR, STDIN, STDOUT,
    };
    use tairix_audio::live::RtAudio;
    use tairix_audio::stream::devices;
    use tairix_audio::target::AudioTarget;
    use tairix_curses::{Input, Screen, Size, StreamTty};
    use tairix_play::command::{parse, Command, Interface, Options, USAGE};
    use tairix_play::report;
    use tairix_play::view::{self, Key};
    use tairix_player::rt::{live, EngineWaits, RtEngine, RtFiles, Tokens};
    use tairix_player::{
        Control, Failure, List, Note, Outcome, Programme, Span, Status, Transport,
    };
    use tairix_rt::io::{write_stderr_line, StdInfo, Stderr, Write};
    use tairix_rt::keys::{KeyRelay, Keys};
    use tairix_rt::sync::{Mutex, WorkerWake};
    use tairix_rt::thread::{JoinHandle, Thread};
    use tairix_sandbox::audiodecode::AudioDecodeService;
    use tairix_sandbox::rt::{serve_session_stdio, session_worker_role};
    use tairix_termcap::{from_term, TermType};

    const DECODER_READ: u64 = 1;
    const DECODER_WRITE: u64 = 2;
    const NOTIFY: u64 = 3;
    const COMMANDS: u64 = 4;
    const SIGNALS: u64 = 5;

    const KEYS: u64 = 1;
    const STATUS: u64 = 2;
    const HANDS: u64 = 3;

    /// The command a suspend key sends: the playback thread pauses, stops
    /// the process, and plays on once it is continued.
    const SUSPEND: u8 = 0xFF;

    /// Commands the interface may have queued before the playback loop
    /// drains them.
    const COMMAND_CAPACITY: usize = 32;

    /// The shortest time between two paints: twenty frames a second is as
    /// fast as a terminal's text is read, and a burst of status changes costs
    /// one paint.
    const PAINT_NS: u64 = 50_000_000;

    /// The grid a terminal whose size the kernel cannot attest is drawn in.
    const FALLBACK: Size = Size::new(24, 80);

    /// Exit statuses: every file played, something was not played, or the
    /// command line was refused.
    const PLAYED: i32 = 0;
    const NOT_PLAYED: i32 = 1;
    const USAGE_ERROR: i32 = 2;

    /// The tokens the playback loop's wait-set reports the engine's sources
    /// under.
    const ENGINE: Tokens = Tokens {
        decoder_read: DECODER_READ,
        decoder_write: DECODER_WRITE,
        notify: NOTIFY,
    };

    /// What the playback thread shows the interface.
    struct Board {
        status: Status,
        notice: Option<String>,
        done: bool,
        /// A wake is on its way that the interface has not yet read the board
        /// for, so a further change needs none.
        woken: bool,
    }

    /// The interface thread and what the playback thread reaches it by.
    struct Ui {
        board: Arc<Mutex<Board>>,
        wake: Arc<WorkerWake>,
        commands: u64,
        thread: JoinHandle<()>,
    }

    /// What the interface thread parks on: its keys, the playback thread's
    /// wake, and the terminal's foreground edge.
    struct Wakes {
        set: u64,
        keys: KeyRelay,
    }

    impl Wakes {
        fn watch(keys: KeyRelay, wake: &WorkerWake) -> Result<Self, Errno> {
            let read = wake.read_end().ok_or(Errno::OutOfMemory)?;
            let created = tairix_rt::waitset_create();
            let set = u64::try_from(created).map_err(|_| Errno::from_syscall(created))?;
            for (kind, id, token) in [
                (WaitSourceKind::Port, keys.port(), KEYS),
                (WaitSourceKind::Stream, u64::from(read), STATUS),
                (WaitSourceKind::Foreground, u64::from(STDIN), HANDS),
            ] {
                let joined = tairix_rt::waitset_ctl(set, WaitSetOp::Add, kind, id, token);
                if joined != 0 {
                    return Err(Errno::from_syscall(joined));
                }
            }
            Ok(Self { set, keys })
        }
    }

    impl Ui {
        /// Start the interface over `list` on a terminal of type `term`.
        fn start(list: List, gain: AudioGain, term: TermType) -> Result<Self, Errno> {
            let wake = Arc::new(WorkerWake::create());
            let wakes = Wakes::watch(KeyRelay::start()?, &wake)?;
            let commands = tairix_rt::bind_private_port(1, COMMAND_CAPACITY)?;
            let board = Arc::new(Mutex::new(Board {
                status: Status::new(gain),
                notice: None,
                done: false,
                woken: false,
            }));
            let shown = Arc::clone(&board);
            let woken = Arc::clone(&wake);
            let thread =
                Thread::spawn(move || interface(&list, &shown, &woken, commands, wakes, term))?;
            Ok(Self {
                board,
                wake,
                commands,
                thread,
            })
        }

        fn publish(&self, status: &Status, notice: Option<String>) {
            let wake = {
                let mut board = self.board.lock();
                board.status = status.clone();
                if notice.is_some() {
                    board.notice = notice;
                }
                !core::mem::replace(&mut board.woken, true)
            };
            if wake {
                self.wake.nudge();
            }
        }

        /// Give the terminal back and wait for the thread to have done so.
        fn finish(self) {
            let wake = {
                let mut board = self.board.lock();
                board.done = true;
                !core::mem::replace(&mut board.woken, true)
            };
            if wake {
                self.wake.nudge();
            }
            let _ = self.thread.join();
        }
    }

    /// The interface thread.
    fn interface(
        list: &List,
        board: &Mutex<Board>,
        wake: &WorkerWake,
        commands: u64,
        wakes: Wakes,
        term: TermType,
    ) {
        let Wakes { set, mut keys } = wakes;
        let mut screen = Screen::new(StreamTty, term, terminal_size());
        let mut input = Input::new();
        let mut shown = false;
        let mut dirty = true;
        let mut painted_at: Option<u64> = None;
        let mut held = tairix_rt::foreground_held(STDIN).unwrap_or(false);
        loop {
            if held && !shown {
                let _ = tairix_rt::set_input_mode(InputMode::Raw);
                shown = screen.enter_full_screen().is_ok();
                dirty = true;
            } else if !held && shown {
                hide(&mut screen);
                shown = false;
            }
            let (status, notice, done) = {
                let mut board = board.lock();
                board.woken = false;
                (board.status.clone(), board.notice.clone(), board.done)
            };
            if done {
                break;
            }
            let now = tairix_rt::clock_get();
            let mut timeout = u64::MAX;
            if shown && dirty {
                let due = painted_at.map_or(0, |at| at.saturating_add(PAINT_NS));
                if now >= due {
                    paint(&mut screen, list, &status, notice.as_deref());
                    painted_at = Some(now);
                    dirty = false;
                } else {
                    timeout = due - now;
                }
            }
            let mut token = 0;
            let parked = tairix_rt::waitset_wait(set, timeout, &mut token);
            if parked != 0 {
                match Errno::from_syscall(parked) {
                    Errno::TimedOut => continue,
                    errno => {
                        if shown {
                            hide(&mut screen);
                        }
                        write_stderr_line(&format!("play: the interface stopped: {errno}"));
                        return;
                    }
                }
            }
            match token {
                KEYS => {
                    while let Some(typed) = keys.take() {
                        let Keys::Typed(bytes) = typed else { continue };
                        let mut wanted = Vec::new();
                        input.feed(bytes, |event| wanted.extend(view::key(&event)));
                        for key in wanted {
                            if key == Key::Suspend {
                                hide(&mut screen);
                                shown = false;
                                held = false;
                            }
                            let _ = tairix_rt::ipc_send(commands, &[command_byte(key)]);
                        }
                    }
                }
                STATUS => {
                    wake.drain();
                    dirty = true;
                }
                _ => held = tairix_rt::foreground_held(STDIN).unwrap_or(false),
            }
        }
        if shown {
            hide(&mut screen);
        }
    }

    fn hide(screen: &mut Screen<StreamTty>) {
        let _ = screen.leave_full_screen();
        let _ = tairix_rt::set_input_mode(InputMode::Cooked);
    }

    fn paint(screen: &mut Screen<StreamTty>, list: &List, status: &Status, notice: Option<&str>) {
        let size = terminal_size();
        if size != screen.size() {
            screen.resize(size);
        }
        let window = view::draw(list, status, notice, size);
        screen.wnoutrefresh(&window);
        let _ = screen.doupdate();
    }

    fn terminal_size() -> Size {
        tairix_rt::terminal_size(STDOUT)
            .map_or(FALLBACK, |grid| Size::new(grid.rows(), grid.cols()))
    }

    const CONTROLS: [Control; 8] = [
        Control::TogglePause,
        Control::Forward,
        Control::Back,
        Control::Next,
        Control::Previous,
        Control::Louder,
        Control::Quieter,
        Control::Stop,
    ];

    fn command_byte(key: Key) -> u8 {
        match key {
            Key::Suspend => SUSPEND,
            Key::Control(control) => CONTROLS
                .iter()
                .position(|known| *known == control)
                .and_then(|at| u8::try_from(at).ok())
                .unwrap_or(SUSPEND),
        }
    }

    /// The playback thread's own mailbox, where only the interface thread
    /// of this process may post.
    struct Commands {
        port: u64,
        pid: u64,
    }

    impl Commands {
        /// Watch `port` on `set`, believing only this process's posts to it.
        fn watch(set: u64, port: u64) -> Result<Self, Errno> {
            let pid = tairix_rt::self_origin().map_err(Errno::from_syscall)?.pid();
            let joined =
                tairix_rt::waitset_ctl(set, WaitSetOp::Add, WaitSourceKind::Port, port, COMMANDS);
            if joined != 0 {
                return Err(Errno::from_syscall(joined));
            }
            Ok(Self { port, pid })
        }

        fn take(&self) -> Option<u8> {
            let mut message = [0u8; 1];
            let mut sender = [0u8; ORIGIN_WIRE_LEN];
            let len = tairix_rt::ipc_recv(self.port, &mut message, &mut sender).ok()?;
            let ours = Origin::from_bytes(&sender).is_ok_and(|origin| origin.pid() == self.pid);
            (ours && len == 1).then_some(message[0])
        }
    }

    /// A one-line progress report on a terminal's standard error, while the
    /// terminal is this process's.
    struct Progress {
        shown: bool,
        second: Option<u64>,
    }

    impl Progress {
        fn update(&mut self, list: &List, status: &Status) {
            let Some((entry, info)) = &status.heard else {
                return;
            };
            let hz = info.rate.hz();
            let here = Span::of_frames(status.position, hz);
            if self.second.replace(here.seconds()) == Some(here.seconds())
                || !tairix_rt::foreground_held(STDERR).unwrap_or(false)
            {
                return;
            }
            let total = info.frames.map_or(String::new(), |frames| {
                format!(" / {}", Span::of_frames(frames, hz).clock())
            });
            let name = list.item(*entry).unwrap_or("");
            let line = format!("\r{}{total}  {name}\x1b[K", here.clock());
            let _ = Stderr.write_all(line.as_bytes());
            self.shown = true;
        }

        /// End the line, so what follows starts on its own.
        fn clear(&mut self) {
            if self.shown {
                let _ = Stderr.write_all(b"\r\x1b[K");
                self.shown = false;
            }
        }
    }

    fn main() -> i32 {
        if session_worker_role() {
            serve_session_stdio(&mut AudioDecodeService::new());
            return PLAYED;
        }
        let Some(arguments) = tairix_rt::args() else {
            write_stderr_line(USAGE);
            return USAGE_ERROR;
        };
        match parse(&arguments) {
            Ok(Command::Help) => tairix_help::print_own_short_help("play", Some(USAGE)),
            Ok(Command::Version) => {
                let line = format!("play (TAIRiX) {}\n", env!("CARGO_PKG_VERSION"));
                match tairix_rt::io::Stdout.write_all(line.as_bytes()) {
                    Ok(()) => PLAYED,
                    Err(_) => NOT_PLAYED,
                }
            }
            Ok(Command::ListDevices) => list_devices(),
            Ok(Command::Play(options)) => play(&options),
            Err(err) => {
                write_stderr_line(&format!("play: {err}"));
                write_stderr_line(USAGE);
                USAGE_ERROR
            }
        }
    }

    /// Every sink the audio service lists to this session.
    fn sinks() -> Result<Vec<AudioDeviceDescriptor>, Errno> {
        devices(&mut RtAudio::new(), StreamDirection::Playback)
    }

    /// The sink `target` names now. A target that names itself needs no
    /// listing; a location is looked up, and one with no device there is a
    /// reason, not a guess.
    fn resolve_sink(target: AudioTarget) -> Result<u32, String> {
        if let Some(device_id) = target.device_id() {
            return Ok(device_id);
        }
        let sinks =
            sinks().map_err(|err| format!("the audio service could not list its sinks: {err}"))?;
        target
            .resolve(&sinks)
            .map(|sink| sink.device_id)
            .ok_or_else(|| format!("{target}: no sink is there"))
    }

    fn list_devices() -> i32 {
        let sinks = match sinks() {
            Ok(sinks) => sinks,
            Err(err) => {
                write_stderr_line(&format!(
                    "play: the audio service could not list its sinks: {err}"
                ));
                return NOT_PLAYED;
            }
        };
        let mut out = String::new();
        for device in &sinks {
            let default = if device.default.is_default() {
                "  (default)"
            } else {
                ""
            };
            let _ = writeln!(
                out,
                "{}\t{}\t{}{default}",
                AudioTarget::of(device),
                AudioTarget::at(device),
                device.name.as_str()
            );
        }
        match tairix_rt::io::Stdout.write_all(out.as_bytes()) {
            Ok(()) => PLAYED,
            Err(_) => NOT_PLAYED,
        }
    }

    fn play(options: &Options) -> i32 {
        let terminal = tairix_rt::foreground_held(STDIN).is_ok();
        let wants_ui = match options.interface {
            Interface::Off => false,
            Interface::Auto => terminal,
            Interface::Forced if terminal => true,
            Interface::Forced => {
                write_stderr_line("play: --ui: standard input is not a terminal to draw on");
                return NOT_PLAYED;
            }
        };
        match Session::new(options, wants_ui) {
            Ok(mut session) => {
                session.serve();
                session.finish()
            }
            Err(reason) => {
                write_stderr_line(&format!("play: {reason}"));
                NOT_PLAYED
            }
        }
    }

    /// One playback and everything that watches it.
    struct Session {
        engine: RtEngine<List, RtFiles>,
        set: u64,
        waits: EngineWaits,
        ui: Option<Ui>,
        commands: Option<Commands>,
        progress: Option<Progress>,
        verbose: bool,
        diagnostics: Vec<String>,
        rate: Option<u32>,
    }

    impl Session {
        fn new(options: &Options, wants_ui: bool) -> Result<Self, String> {
            let term = tairix_rt::env_var(b"TERM")
                .and_then(|raw| core::str::from_utf8(raw).ok())
                .map_or(TermType::Dumb, from_term);
            let (quiet, verbose, gain) = (options.quiet, options.verbose, options.gain);
            let (list, settings) = options.playback(resolve_sink(options.target)?);
            let engine = live(list.clone(), settings, RtFiles::new())
                .map_err(|failure| format!("{failure}"))?;
            let set = u64::try_from(tairix_rt::waitset_create())
                .map_err(|_| String::from("there is no wait-set to play from"))?;
            // An intake no wait-set watches would swallow the signals that end
            // the process.
            if tairix_rt::signal_intake(SignalIntakeOp::Enable) == 0 {
                let joined =
                    tairix_rt::waitset_ctl(set, WaitSetOp::Add, WaitSourceKind::Signal, 0, SIGNALS);
                if joined != 0 {
                    return Err(format!(
                        "signals could not be watched: {}",
                        Errno::from_syscall(joined)
                    ));
                }
            }
            let ui = if wants_ui {
                Some(
                    Ui::start(list, gain, term)
                        .map_err(|err| format!("the interface could not start: {err}"))?,
                )
            } else {
                None
            };
            let commands = match &ui {
                Some(ui) => Some(Commands::watch(set, ui.commands).map_err(|err| {
                    format!("the interface's commands could not be watched: {err}")
                })?),
                None => None,
            };
            let progress = (!quiet && ui.is_none()).then_some(Progress {
                shown: false,
                second: None,
            });
            Ok(Self {
                engine,
                set,
                waits: EngineWaits::new(set, ENGINE),
                ui,
                commands,
                progress,
                verbose,
                diagnostics: Vec::new(),
                rate: None,
            })
        }

        /// Play until the engine is done.
        fn serve(&mut self) {
            self.engine.begin(tairix_rt::clock_get());
            loop {
                self.show();
                if self.engine.outcome().is_some() {
                    return;
                }
                let now = tairix_rt::clock_get();
                let timeout = self
                    .engine
                    .deadline()
                    .map_or(u64::MAX, |at| at.saturating_sub(now));
                let mut token = 0;
                let waited = tairix_rt::waitset_wait(self.set, timeout, &mut token);
                if waited != 0 {
                    match Errno::from_syscall(waited) {
                        Errno::TimedOut => self.engine.on_timer(tairix_rt::clock_get()),
                        errno => self.engine.abandon(Failure::Wait {
                            what: "wait",
                            errno,
                        }),
                    }
                    continue;
                }
                let now = tairix_rt::clock_get();
                if self.waits.deliver(&mut self.engine, token, now) {
                    continue;
                }
                match token {
                    COMMANDS => {
                        while let Some(command) = self.commands.as_ref().and_then(Commands::take) {
                            obey(&mut self.engine, command);
                        }
                    }
                    SIGNALS => {
                        let drained = tairix_rt::signal_intake(SignalIntakeOp::Take);
                        if u32::try_from(drained)
                            .ok()
                            .and_then(|raw| Signal::from_u32(raw).ok())
                            .is_some()
                        {
                            self.engine.on_control(now, Control::Stop);
                        }
                    }
                    _ => {}
                }
            }
        }

        /// Bring the wait-set, the records and the interface up to date with
        /// the engine.
        fn show(&mut self) {
            self.waits.sync(&mut self.engine);
            for note in self.engine.take_notes() {
                let notice = self.report(note);
                if let (Some(ui), Some(notice)) = (&self.ui, notice) {
                    ui.publish(self.engine.status(), Some(notice));
                }
            }
            if self.engine.take_changed() {
                match (&self.ui, &mut self.progress) {
                    (Some(ui), _) => ui.publish(self.engine.status(), None),
                    (None, Some(progress)) => {
                        progress.update(self.engine.programme(), self.engine.status());
                    }
                    (None, None) => {}
                }
            }
        }

        /// Say what `note` means on fd 3 and standard error, answering the
        /// line the interface shows, if any.
        fn report(&mut self, note: Note) -> Option<String> {
            let deferred = self.ui.is_some();
            let (Note::Opened { entry, .. }
            | Note::Skipped { entry, .. }
            | Note::Cut { entry, .. }) = note;
            let file = String::from(self.engine.programme().item(entry).unwrap_or(""));
            match note {
                Note::Opened { info, .. } => {
                    self.rate = Some(info.rate.hz());
                    let _ = StdInfo.write_all(&report::opened(&file, &info));
                    if self.verbose && !deferred {
                        self.clear_progress();
                        write_stderr_line(&format!("play: {file}: {}", report::describe(&info)));
                    }
                    None
                }
                Note::Skipped { why, .. } | Note::Cut { why, .. } => {
                    let whole = matches!(note, Note::Skipped { .. });
                    let _ = StdInfo.write_all(&report::left_out(&file, why, whole));
                    let line = format!("play: {file}: {why}");
                    if !deferred {
                        self.clear_progress();
                        write_stderr_line(&line);
                    }
                    self.diagnostics.push(line.clone());
                    Some(line)
                }
            }
        }

        fn clear_progress(&mut self) {
            if let Some(progress) = &mut self.progress {
                progress.clear();
            }
        }

        /// Give the terminal back, say how it went, and answer the exit
        /// status.
        fn finish(mut self) -> i32 {
            if let Some(ui) = self.ui.take() {
                ui.finish();
                for line in &self.diagnostics {
                    write_stderr_line(line);
                }
            }
            self.clear_progress();
            let outcome = self.engine.outcome().unwrap_or(Outcome::Stopped);
            let _ = StdInfo.write_all(&report::summary(self.engine.status(), self.rate, outcome));
            match outcome {
                Outcome::Failed(failure) => {
                    write_stderr_line(&format!("play: {failure}"));
                    NOT_PLAYED
                }
                Outcome::Played | Outcome::Stopped if self.diagnostics.is_empty() => PLAYED,
                Outcome::Played | Outcome::Stopped => NOT_PLAYED,
            }
        }
    }

    /// Carry out one command the interface sent.
    fn obey(engine: &mut RtEngine<List, RtFiles>, command: u8) {
        let now = tairix_rt::clock_get();
        if command != SUSPEND {
            if let Some(control) = CONTROLS.get(usize::from(command)) {
                engine.on_control(now, *control);
            }
            return;
        }
        let playing = matches!(
            engine.status().transport,
            Transport::Playing | Transport::Starting | Transport::Ending
        );
        if playing {
            engine.on_control(now, Control::TogglePause);
        }
        if let Err(err) = tairix_rt::stop_self() {
            write_stderr_line(&format!("play: could not stop: {err}"));
        }
        if playing {
            engine.on_control(tairix_rt::clock_get(), Control::TogglePause);
        }
    }

    tairix_rt::entry!(main);
}

#[cfg(not(all(freestanding, feature = "program")))]
fn main() {}

//! The `beeb.app` bundle's `Run`: the BBC Micro's window, and, re-entered as
//! a capability-empty sandbox worker, the machine itself.
//!
//! The front end holds the files, the window, the sound stream and the
//! settings, and drives the machine through `tairix_beeb::worker`; the worker
//! holds the ROM images, the media and the guest, and nothing else. On the
//! host it is an inert stub.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
extern crate alloc;

#[cfg(freestanding)]
mod program {
    use alloc::boxed::Box;
    use alloc::collections::VecDeque;
    use alloc::format;
    use alloc::string::String;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::cell::Cell;
    use core::num::NonZeroUsize;

    use tairix_abi::appdata_ipc::BlobMode;
    use tairix_abi::audio::{OpenParams, StreamRole, Tenancy};
    use tairix_abi::driver::audio::{ChannelMap, Frames, Rate, SampleFormat, StreamDirection};
    use tairix_abi::driver::display::DamageRect;
    use tairix_abi::input::PointerButtonCode;
    use tairix_abi::latency::DEFAULT_FRAME_BUDGET_NS;
    use tairix_abi::process::{DOCUMENT_ROLE_ARG, DOCUMENT_WRITABLE_ROLE_ARG, STDIN};
    use tairix_abi::waitset::{WaitSetOp, WaitSourceKind};
    use tairix_abi::window_ipc::{
        AppBar, AppBarClick, AppMenu, ClipboardKind, DocumentName, KeyboardMode, MenuOutcome,
        PickPurpose, PointerAction, SaveFormats, WindowEvent, WindowRegion, WindowSizeState,
    };
    use tairix_abi::{Errno, ProcId};
    use tairix_appdata::{Opening, Publication, PublishWorker, Refusal, RtHost, Settings};
    use tairix_audio::live::{LiveStream, RtAudio};
    use tairix_beeb::front::keys::{self, Keyboard, Press};
    use tairix_beeb::front::media::{
        self, Chosen, Disc, FileWork, Handover, Holding, Purpose, Saving, Shelf, Tape,
    };
    use tairix_beeb::front::menu::{self, Command, Shown};
    use tairix_beeb::front::pacing::{Next, Pacer, Ring};
    use tairix_beeb::front::picture::{expand, palette};
    use tairix_beeb::front::settings::{Image, Preferences, Shipped, INSTALLED};
    use tairix_beeb::front::sheet::{Sheet, Socket};
    use tairix_beeb::front::{events, LEAST_PICTURE, OPEN_PICTURE};
    use tairix_beeb::machine::fdc::PhysicalDrive;
    use tairix_beeb::machine::keyboard::Key;
    use tairix_beeb::machine::video::{CANVAS_HEIGHT, CANVAS_WIDTH};
    use tairix_beeb::machine::CLOCK;
    use tairix_beeb::worker::protocol::{
        session_bounds, Change, Configuration, Input, Lights, Loaded, Media, Ran,
        Refusal as Refused, Reply, Request, Slot, MAX_BULK, MAX_DISC, MAX_INPUTS, MAX_ROM,
        SAMPLE_RATE, SHARE_BYTES, SLOTS,
    };
    use tairix_beeb::worker::{MachineClient, MachineService};
    use tairix_browse::media::save_format;
    use tairix_browse::MediaType;
    use tairix_controls::{StatusBand, StatusTone};
    use tairix_crt::screen::{fit, Drawing, Picture, DOT};
    use tairix_crt::Tube;
    use tairix_geometry::{Rect, Region};
    use tairix_icon::NoArtwork;
    use tairix_raster::resample::Region as Target;
    use tairix_raster::{Color, Surface};
    use tairix_rt::sync::WorkerWake;
    use tairix_rt::work::{Worker, WorkerGuard};
    use tairix_rt::{File, LogSink};
    use tairix_sandbox::rt::{
        serve_session_stdio, session_worker_role, spawn_share, RtSessionLauncher, SessionMembers,
    };
    use tairix_sandbox::session::Share;
    use tairix_sandbox::supervise::send_waiting;
    use tairix_sandbox::{SupervisedSession, WorkerMemory};
    use tairix_theme::{Theme, ThemeRegistry};
    use tairix_util::defer::JobQueue;
    use tairix_window::app::{self, Fitted, LazyPool, Wake, WindowPane};
    use tairix_window::docapp::settings::{Edit, SettingsRequest, Then};
    use tairix_window::docapp::{AppRequest, AppView};
    use tairix_window::{
        is_quit, key_input_event, pointer_input_events, pointer_point, Answer, Desktop, EventDrain,
        EventError, EventMailbox, EventSource, Owed, Parked, Repaint, Target as Opened,
        WindowClient, WindowEvents, WindowSizing,
    };

    /// The name this program states its refusals under: its manifest's.
    const APP_NAME: &str = "beeb";
    const TITLE: &str = "BBC Micro";

    const SESSION_READ: u64 = app::FIRST_APP_TOKEN;
    const SESSION_WRITE: u64 = SESSION_READ + 1;
    const SOUND: u64 = SESSION_WRITE + 1;
    const FILES: u64 = SOUND + 1;
    const SETTINGS: u64 = FILES + 1;

    /// The sound asked to be queued ahead of the device, and the least kept.
    const LATENCY_FRAMES: u32 = 2_400;
    const LEAST_RING: u32 = 1_920;
    /// File jobs waiting at once.
    const FILE_JOBS: usize = 16;
    /// The least a key is held, in cycles: long enough for two keyboard scans.
    const LEAST_HOLD: u64 = 50_000;
    /// Cycles from one pasted character's key to the next's.
    const TYPE_GAP: u64 = 100_000;
    /// The most pasted text taken at once.
    const MOST_PASTE: usize = 4_096;
    /// The scan lines the picture's tube draws when they are on.
    const SCAN_LINES: Tube = Tube {
        scanlines: 450,
        glow: 0,
        fuzz: 0,
        phosphor: 0,
        wobble: 0,
    };

    type Session = SupervisedSession<RtSessionLauncher, LogSink>;
    type Files = Worker<Held, Job, Done, JobQueue<Job, Done>>;

    /// Work done off the loop: every file read and written, and the sound
    /// stream's opening.
    enum Job {
        /// The ROM images the settings fit, from the bundle and the store.
        Roms(Box<Preferences>),
        /// A file handed over, read whole and kept to be written back to.
        Take {
            grant: u64,
            writable: bool,
            name: String,
            purpose: Purpose,
        },
        /// The file the application was opened with.
        Launched {
            file: Option<File>,
            writable: bool,
            name: String,
        },
        /// An image written back over the file a medium came from.
        WriteBack { medium: u32, bytes: Vec<u8> },
        /// Bytes written into a file chosen to hold them.
        SaveTo { grant: u64, bytes: Vec<u8> },
        /// The file a medium came from let go.
        Forget(u32),
        /// The sound stream opened.
        Sound,
    }

    /// What a [`Job`] came to.
    enum Done {
        Roms(Result<(Box<Images>, Box<Preferences>), String>),
        Taken {
            purpose: Purpose,
            name: String,
            bytes: Vec<u8>,
            medium: Option<u32>,
            writable: bool,
        },
        Installed {
            socket: Socket,
            blob: String,
        },
        Written(Result<(), String>),
        Sound(Option<Speaker>),
        Nothing,
        Failed(String),
    }

    /// The file worker's own state: the files the media came from.
    struct Held {
        next: u32,
        files: Vec<(u32, File)>,
    }

    /// The images a machine is built from, read.
    struct Images {
        os: Vec<u8>,
        slots: [Fitting; SLOTS],
    }

    enum Fitting {
        Empty,
        Ram,
        Rom(Vec<u8>),
    }

    impl Images {
        fn configuration(&self, links: u8) -> Configuration<'_> {
            let mut slots = [Slot::Empty; SLOTS];
            for (slot, fitted) in slots.iter_mut().zip(&self.slots) {
                *slot = match fitted {
                    Fitting::Empty => Slot::Empty,
                    Fitting::Ram => Slot::Ram,
                    Fitting::Rom(image) => Slot::Rom(image),
                };
            }
            Configuration {
                os: &self.os,
                slots,
                links,
            }
        }
    }

    fn serve_files(held: &mut Held, job: &mut Job) -> Done {
        match job {
            Job::Roms(preferences) => Done::Roms(
                read_roms(preferences)
                    .map(|images| (Box::new(images), core::mem::take(preferences))),
            ),
            Job::Take {
                grant,
                writable,
                name,
                purpose,
            } => match File::from_delegation(*grant) {
                Ok(file) => keep(held, file, *writable, core::mem::take(name), *purpose),
                Err(err) => Done::Failed(format!(
                    "{name} could not be taken ({})",
                    Errno::from_syscall(err)
                )),
            },
            Job::Launched {
                file,
                writable,
                name,
            } => match (file.take(), Purpose::of(name)) {
                (Some(file), Some(purpose)) => {
                    keep(held, file, *writable, core::mem::take(name), purpose)
                }
                _ => Done::Failed(format!("{name} is not a disc or tape image")),
            },
            Job::WriteBack { medium, bytes } => Done::Written(
                held.files
                    .iter()
                    .find(|(id, _)| id == medium)
                    .ok_or_else(|| String::from("its file is no longer held"))
                    .and_then(|(_, file)| overwrite(file, bytes)),
            ),
            Job::SaveTo { grant, bytes } => Done::Written(
                File::from_delegation(*grant)
                    .map_err(|err| {
                        format!("the file could not be taken ({})", Errno::from_syscall(err))
                    })
                    .and_then(|file| overwrite(&file, bytes)),
            ),
            Job::Forget(medium) => {
                held.files.retain(|(id, _)| id != medium);
                Done::Nothing
            }
            Job::Sound => Done::Sound(Speaker::open()),
        }
    }

    /// `file` read whole, kept to be written back to while `writable`; a ROM
    /// image is copied into the store instead.
    fn keep(held: &mut Held, file: File, writable: bool, name: String, purpose: Purpose) -> Done {
        let most = match purpose {
            Purpose::Disc(_) => MAX_DISC,
            Purpose::Rom(_) => MAX_ROM,
            Purpose::Tape | Purpose::Snapshot => MAX_BULK,
        };
        let bytes = match read_whole(&file, most) {
            Ok(bytes) => bytes,
            Err(why) => return Done::Failed(format!("{name} could not be read: {why}")),
        };
        if let Purpose::Rom(socket) = purpose {
            return install(socket, &name, &bytes);
        }
        let medium = (writable && matches!(purpose, Purpose::Disc(_) | Purpose::Tape)).then(|| {
            let id = held.next;
            held.next = held.next.wrapping_add(1);
            held.files.push((id, file));
            id
        });
        Done::Taken {
            purpose,
            name,
            bytes,
            medium,
            writable,
        }
    }

    /// `bytes` kept in the store as an installed ROM, its blob named from
    /// `name`.
    fn install(socket: Socket, name: &str, bytes: &[u8]) -> Done {
        let stem: String = name
            .rsplit_once('.')
            .map_or(name, |(stem, _)| stem)
            .chars()
            .filter_map(|c| match c.to_ascii_lowercase() {
                c @ ('a'..='z' | '0'..='9') => Some(c),
                ' ' | '-' | '_' | '.' => Some('-'),
                _ => None,
            })
            .take(40)
            .collect();
        let stem = stem.trim_matches('-');
        let blob = format!(
            "{INSTALLED}{}",
            if stem.is_empty() { "image" } else { stem }
        );
        let written = tairix_appdata::blobs::open(&mut RtHost, &blob, BlobMode::ReadWrite)
            .map_err(|err| format!("the store refused it ({err})"))
            .and_then(|grant| {
                File::from_delegation(grant).map_err(|err| {
                    format!(
                        "the store's file could not be taken ({})",
                        Errno::from_syscall(err)
                    )
                })
            })
            .and_then(|file| overwrite(&file, bytes));
        match written {
            Ok(()) => Done::Installed { socket, blob },
            Err(why) => Done::Failed(format!("{name} could not be installed: {why}")),
        }
    }

    fn read_whole(file: &File, most: usize) -> Result<Vec<u8>, String> {
        let len = file
            .regular_len()
            .map_err(|err| format!("{err}"))?
            .ok_or_else(|| String::from("it is not a file"))?;
        let len = usize::try_from(len)
            .ok()
            .filter(|&len| len <= most)
            .ok_or_else(|| format!("it is larger than {most} bytes"))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(len)
            .map_err(|_| String::from("there is not enough memory"))?;
        bytes.resize(len, 0);
        let mut at = 0;
        while at < len {
            let read = file
                .read_at(at as u64, &mut bytes[at..])
                .map_err(|err| format!("{}", Errno::from_syscall(err)))?;
            if read == 0 {
                break;
            }
            at += read;
        }
        bytes.truncate(at);
        Ok(bytes)
    }

    fn overwrite(file: &File, bytes: &[u8]) -> Result<(), String> {
        let mut at = 0;
        while at < bytes.len() {
            let wrote = file
                .write_at(at as u64, &bytes[at..])
                .map_err(|err| format!("{}", Errno::from_syscall(err)))?;
            if wrote == 0 {
                return Err(String::from("there is no room for it"));
            }
            at += wrote;
        }
        file.truncate(at as u64).map_err(|err| format!("{err}"))?;
        file.sync().map_err(|err| format!("{err}"))
    }

    /// The images `preferences` fits: the shipped ROMs `Run` carries, the
    /// installed ones from the store.
    fn read_roms(preferences: &Preferences) -> Result<Images, String> {
        let os = match &preferences.os {
            Image::Shipped(shipped) => shipped_rom(*shipped)?,
            Image::Installed(blob) => installed_rom(blob)?,
            Image::Empty | Image::Ram => return Err(String::from("there is no OS")),
        };
        let mut slots: [Fitting; SLOTS] = core::array::from_fn(|_| Fitting::Empty);
        for (fitted, image) in slots.iter_mut().zip(&preferences.slots) {
            *fitted = match image {
                Image::Empty => Fitting::Empty,
                Image::Ram => Fitting::Ram,
                Image::Shipped(shipped) => Fitting::Rom(shipped_rom(*shipped)?),
                Image::Installed(blob) => Fitting::Rom(installed_rom(blob)?),
            };
        }
        Ok(Images { os, slots })
    }

    /// A shipped ROM's image, copied to be handed to the worker.
    fn shipped_rom(shipped: Shipped) -> Result<Vec<u8>, String> {
        let image = shipped.image();
        let mut copy = Vec::new();
        copy.try_reserve_exact(image.len())
            .map_err(|_| String::from("there is not enough memory"))?;
        copy.extend_from_slice(image);
        Ok(copy)
    }

    fn installed_rom(blob: &str) -> Result<Vec<u8>, String> {
        let grant = tairix_appdata::blobs::open(&mut RtHost, blob, BlobMode::Read)
            .map_err(|err| format!("{blob}: {err}"))?;
        let file = File::from_delegation(grant)
            .map_err(|err| format!("{blob}: {}", Errno::from_syscall(err)))?;
        read_whole(&file, MAX_ROM).map_err(|why| format!("{blob}: {why}"))
    }

    /// The open sound stream, and how full its ring is kept.
    struct Speaker {
        audio: RtAudio,
        stream: LiveStream,
        target: u32,
    }

    impl Speaker {
        fn open() -> Option<Self> {
            let mut audio = RtAudio::new();
            let params = OpenParams {
                device_id: 0,
                direction: StreamDirection::Playback,
                format: SampleFormat::S16,
                rate: Rate::new(SAMPLE_RATE).ok()?,
                channel_map: ChannelMap::MONO,
                role: StreamRole::Media,
                latency_target_frames: LATENCY_FRAMES,
                tenancy: Tenancy::Caller,
            };
            let mut stream = match LiveStream::open(&mut audio, &params) {
                Ok(stream) => stream,
                Err(why) => {
                    app::report(
                        APP_NAME,
                        format_args!("no sound ({why:?}); the machine is silent"),
                    );
                    return None;
                }
            };
            let grant = stream.client().grant();
            if grant.format != SampleFormat::S16
                || grant.channel_map != ChannelMap::MONO
                || grant.rate.hz() != SAMPLE_RATE
            {
                app::report(
                    APP_NAME,
                    "the sound stream is not 48 kHz 16-bit mono; the machine is silent",
                );
                let _ = stream.close(&mut audio);
                return None;
            }
            let target = grant
                .granted_latency_frames
                .max(LEAST_RING)
                .min(grant.ring_frames);
            if let Err(err) = stream.client_mut().start(&mut audio, Frames::new(0)) {
                app::report(
                    APP_NAME,
                    format_args!("the sound stream would not start ({err})"),
                );
                let _ = stream.close(&mut audio);
                return None;
            }
            Some(Self {
                audio,
                stream,
                target,
            })
        }

        fn ring(&mut self) -> Option<Ring> {
            let queued = self.stream.ring().ok()?.readable_frames().ok()?;
            Some(Ring {
                queued,
                target: self.target,
            })
        }

        /// `samples` queued after what is already queued, at `volume`
        /// thousandths of their level.
        fn play(
            &mut self,
            samples: &[u8],
            volume: u32,
            scratch: &mut Vec<u8>,
        ) -> Result<(), Errno> {
            let volume = i32::try_from(volume).unwrap_or(0);
            scratch.clear();
            scratch.extend(samples.as_chunks::<2>().0.iter().flat_map(|&pair| {
                let level = i32::from(i16::from_le_bytes(pair)) * volume / 1000;
                i16::try_from(level).unwrap_or(0).to_le_bytes()
            }));
            let at = self.stream.ring()?.producer_position()?;
            self.stream.write_at(at, scratch).map(drop)
        }

        fn drain(&mut self) {
            while let Ok(Some(_)) = self.stream.take_notify(&mut self.audio) {}
        }
    }

    /// What woke the loop, a bit each.
    #[derive(Clone, Copy, Default)]
    struct Woke(u8);

    impl Woke {
        const DESKTOP: u8 = 1;
        const READABLE: u8 = 2;
        const WRITABLE: u8 = 4;
        const SOUND: u8 = 8;
        const FILES: u8 = 16;
        const SETTINGS: u8 = 32;

        const fn has(self, bit: u8) -> bool {
            self.0 & bit != 0
        }
    }

    struct RtEventSource<'a> {
        mailbox: EventMailbox,
        set: u64,
        files: &'a Files,
        settings: &'a PublishWorker<Preferences>,
        due: &'a Cell<Option<u64>>,
        woke: &'a Cell<Woke>,
    }

    impl EventDrain for RtEventSource<'_> {
        fn try_next(&mut self, event: &mut [u8; WindowEvent::WIRE_LEN]) -> Result<bool, Errno> {
            self.mailbox.try_next(event)
        }
    }

    impl EventSource for RtEventSource<'_> {
        fn park(&mut self) -> Result<Parked, Errno> {
            let mut woke = self.woke.get();
            let parked = match app::park_until_due(self.set, self.due.get())? {
                None => Parked::Interrupted,
                Some(Wake::App(token)) => {
                    woke.0 |= match token {
                        SESSION_READ => Woke::READABLE,
                        SESSION_WRITE => Woke::WRITABLE,
                        SOUND => Woke::SOUND,
                        // A worker's readiness is a level, so it is taken here or the
                        // park would spin on it.
                        FILES => {
                            self.files.wake().drain();
                            Woke::FILES
                        }
                        SETTINGS => {
                            self.settings.wake().drain();
                            Woke::SETTINGS
                        }
                        _ => 0,
                    };
                    Parked::Interrupted
                }
                Some(Wake::DesktopChanged) => {
                    woke.0 |= Woke::DESKTOP;
                    Parked::Interrupted
                }
                Some(Wake::PressureChanged) => {
                    tairix_font::trim_glyph_cache();
                    Parked::Served
                }
                Some(Wake::Event | Wake::PressureUnchanged) => Parked::Served,
            };
            self.woke.set(woke);
            Ok(parked)
        }
    }

    /// A request waiting for the machine, owning what it carries.
    enum Pending {
        Configure,
        /// A disc into a drive, its record taking the drive once the disc it
        /// replaces is dealt with.
        InsertDisc {
            drive: PhysicalDrive,
            write_protected: bool,
            disc: Disc,
        },
        EjectDisc(PhysicalDrive),
        InsertTape(Tape),
        EjectTape,
        RewindTape,
        RecordTape(bool),
        PowerOn,
        TakeSnapshot,
        RestoreSnapshot(Vec<u8>),
        CollectDiscs,
    }

    /// The settings window.
    struct SheetWindow {
        pane: WindowPane,
        surface: Surface,
        sheet: Sheet,
    }

    struct Front {
        client: WindowClient<app::RtWindowTransport>,
        pane: Option<WindowPane>,
        surface: Surface,
        owed: Owed,
        endpoint: u64,
        set: u64,
        desktop: Desktop,
        themes: ThemeRegistry,
        session: Session,
        members: SessionMembers,
        machine: MachineClient,
        share: Option<Arc<dyn Share>>,
        /// What the request in flight asked.
        asked: Option<Pending>,
        pending: VecDeque<Pending>,
        /// Input changes not yet sent, at their machine cycle.
        inputs: VecDeque<(u64, Change)>,
        /// The machine cycles asked for so far.
        timeline: u64,
        /// The cycle each BBC key last went down at.
        pressed_at: [u64; 0x80],
        pacer: Pacer,
        keyboard: Keyboard,
        images: Option<Box<Images>>,
        /// The settings the machine was built from.
        built: Option<Box<Preferences>>,
        preferences: Publication<Preferences>,
        refusals: Vec<Refusal>,
        publisher: Arc<PublishWorker<Preferences>>,
        files: Arc<Files>,
        sound: Option<Speaker>,
        scratch: Vec<u8>,
        /// The media's records, the picker and the saves waiting for it.
        shelf: Shelf,
        /// The file a picker picked, held while its name is asked.
        picked: Option<(u64, bool)>,
        lights: Lights,
        drives: [bool; 2],
        loaded: Loaded,
        picture: Picture,
        palette: [[u8; DOT]; 256],
        slots: Vec<u8>,
        pool: LazyPool,
        menu_open: Option<u64>,
        bar_revision: u32,
        shown: Option<Shown>,
        sheet: Option<SheetWindow>,
        message: Option<(String, StatusTone)>,
        full_screen: bool,
        damage: Region,
        quitting: bool,
        /// Where the machine's timeline stood when it was last built, until
        /// its first second has run.
        running_from: Option<u64>,
    }

    /// Record `id` with `message` in the system log, best-effort.
    fn record(id: tairix_log::EventId, message: &str) {
        tairix_log::log(
            &LogSink,
            &tairix_log::Event {
                level: tairix_log::Level::Info,
                id,
                message,
                fields: &[],
            },
        );
    }

    impl Front {
        fn theme(&self) -> &Theme {
            self.themes.active()
        }

        fn window_id(&self) -> Option<u64> {
            self.pane.as_ref().map(WindowPane::id)
        }

        fn mode(&self) -> Option<tairix_abi::driver::display::DisplayMode> {
            self.pane.as_ref().map(|pane| *pane.mode())
        }

        /// The window's picture and status strip.
        fn layout(&self) -> Option<(Target, Rect)> {
            let mode = self.mode()?;
            let band = StatusBand::new(self.theme(), self.desktop.scale());
            let strip = band.height().min(mode.height_px);
            let room = mode.height_px - strip;
            let picture = fit(mode.width_px, room)?;
            Some((
                picture,
                Rect::new(0, i32::try_from(room).ok()?, mode.width_px, strip),
            ))
        }

        fn say(&mut self, text: impl Into<String>, tone: StatusTone) {
            let text = text.into();
            app::report(APP_NAME, &text);
            self.message = Some((text, tone));
            self.owe_strip();
        }

        /// State a refusal of something the user asked, in an alert and the
        /// status strip.
        fn refuse(&mut self, headline: &str, detail: &str) {
            let owner = self.window_id();
            app::refuse(
                &mut self.client,
                (APP_NAME, self.endpoint),
                owner,
                headline,
                detail,
            );
            self.message = Some((String::from(headline), StatusTone::Danger));
            self.owe_strip();
        }

        fn owe_strip(&mut self) {
            if let Some((_, strip)) = self.layout() {
                self.damage.add(strip);
            }
        }

        /// The picture owed again where the tube moved.
        fn owe_picture(&mut self) {
            if let Some((target, _)) = self.layout() {
                if let (Ok(x), Ok(y)) = (i32::try_from(target.x), i32::try_from(target.y)) {
                    self.damage
                        .add(Rect::new(x, y, target.width, target.height));
                }
            }
        }

        fn owe_whole(&mut self) {
            if let Some(mode) = self.mode() {
                self.damage
                    .add(Rect::new(0, 0, mode.width_px, mode.height_px));
            }
            self.picture.invalidate();
        }

        /// Build the machine from the settings, once its images are read.
        fn build(&mut self) {
            let job = Job::Roms(Box::new(self.preferences.live().clone()));
            self.submit(job, "The machine could not be built");
        }

        /// `job` handed to the file worker; refused, the user is told under
        /// `headline` and any file grant it carries is let go.
        fn submit(&mut self, job: Job, headline: &str) {
            if let Err(job) = self.files.submit(job) {
                if let Job::Take { grant, .. } | Job::SaveTo { grant, .. } = job {
                    drop(File::from_delegation(grant));
                }
                self.refuse(headline, "Too much is waiting on files.");
            }
        }

        fn ask(&mut self, pending: Pending) {
            self.pending.push_back(pending);
        }

        /// The next request waiting, or the slice the pace asks for.
        fn pump(&mut self, now: u64) {
            if !self.machine.is_idle() || !self.session.is_live() {
                return;
            }
            let links = self.preferences.live().links();
            let collect = self.shelf.filed();
            while let Some(pending) = self.pending.pop_front() {
                if let Some(request) = request_of(&pending, self.images.as_deref(), links, collect)
                {
                    if self.machine.ask(&request).is_ok() {
                        self.asked = Some(pending);
                    }
                    return;
                }
            }
            if self.images.is_none() || self.quitting {
                return;
            }
            let hurried = self.preferences.live().fast_tape && self.lights.motor;
            let ring = self.sound.as_mut().and_then(Speaker::ring);
            if let Next::Run { cycles, sound } = self.pacer.next(now, ring, hurried) {
                let end = self.timeline + u64::from(cycles);
                let mut inputs = Vec::new();
                while let Some(&(at, change)) = self.inputs.front() {
                    if at >= end || inputs.len() == MAX_INPUTS {
                        break;
                    }
                    self.inputs.pop_front();
                    let at = u32::try_from(at.saturating_sub(self.timeline)).unwrap_or(0);
                    inputs.push(Input { at, change });
                }
                let request = Request::Run {
                    cycles,
                    sound: sound && self.sound.is_some(),
                    inputs,
                };
                if self.machine.ask(&request).is_ok() {
                    self.timeline = end;
                }
            }
        }

        /// Serve the session as its pipes are ready: what it can take, what it
        /// answered, and the loss of a worker that died. A read is made only
        /// on readiness, as it blocks.
        fn serve_session(&mut self, now: u64, (readable, writable): (bool, bool)) {
            if writable && self.session.wants_write() {
                if let Err(err) = self.session.on_writable(now) {
                    self.lost(&format!("{err:?}"));
                }
            }
            if readable && self.session.wants_read() {
                if let Err(err) = self.session.on_readable(now) {
                    self.lost(&format!("{err:?}"));
                }
            }
            loop {
                let machine = &mut self.machine;
                let got = self.session.recv(now, |frame| match machine.answer(frame) {
                    Ok(_) => Ok(frame.to_vec()),
                    Err(why) => Err(why),
                });
                match got {
                    Ok(Some(Ok(frame))) => self.answered(&frame),
                    Ok(Some(Err(why))) => {
                        self.session.condemn(now, why);
                        self.lost(why);
                    }
                    Ok(None) => break,
                    Err(err) => {
                        self.lost(&format!("{err:?}"));
                        break;
                    }
                }
            }
        }

        /// The worker is gone; a replacement starts from power-on.
        fn lost(&mut self, why: &str) {
            if self.share.take().is_some() {
                record(events::WORKER_REPLACED, events::WORKER_REPLACED_MESSAGE);
                self.say(
                    format!("The machine stopped ({why}); it is starting again."),
                    StatusTone::Warning,
                );
            }
            self.machine.restart();
            self.asked = None;
            self.pending.clear();
            self.inputs.clear();
            self.pressed_at = [0; 0x80];
            self.loaded = Loaded::default();
        }

        /// A replacement worker, once its deadline comes: the machine built
        /// again, its media put back.
        fn restart_due(&mut self, now: u64) {
            if self.quitting || self.session.restart_deadline().is_none_or(|at| at > now) {
                return;
            }
            if self.session.start(now).is_none() {
                return;
            }
            self.share = self.session.share().cloned();
            self.timeline = 0;
            if self.images.is_some() {
                self.ask(Pending::Configure);
            }
            for drive in PhysicalDrive::ALL {
                if let Some(disc) = self.shelf.disc(drive).cloned() {
                    self.ask(Pending::InsertDisc {
                        drive,
                        write_protected: disc.medium.is_none(),
                        disc,
                    });
                }
            }
            if let Some(tape) = self.shelf.tape().cloned() {
                self.ask(Pending::InsertTape(tape));
            }
        }

        fn answered(&mut self, frame: &[u8]) {
            let asked = self.asked.take();
            let Ok(reply) = Reply::decode(frame) else {
                return;
            };
            match reply {
                Reply::Ran(ran) => self.ran(&ran),
                Reply::Media(media) => self.media(&media, asked),
                Reply::Snapshot(snapshot) => self.offer_save(Saving {
                    suggested: String::from("BBC Micro.snapshot"),
                    holding: Holding::Snapshot,
                    bytes: snapshot.to_vec(),
                }),
                Reply::Refused(refusal) => self.refused(refusal, asked),
                Reply::Done => {
                    if matches!(asked, Some(Pending::Configure | Pending::PowerOn)) {
                        self.running_from = Some(self.timeline);
                        self.owe_whole();
                    }
                }
            }
        }

        fn refused(&mut self, refusal: Refused, asked: Option<Pending>) {
            let what = match asked {
                Some(Pending::Configure) => "The machine could not be built",
                Some(Pending::InsertDisc { disc, .. }) => {
                    self.forget(disc.medium);
                    "The disc could not be put in"
                }
                Some(Pending::InsertTape(tape)) => {
                    self.forget(tape.medium);
                    "The tape could not be put in"
                }
                Some(Pending::RestoreSnapshot(_)) => "The snapshot could not be restored",
                Some(Pending::TakeSnapshot) => "The snapshot could not be taken",
                _ => "The machine refused",
            };
            let detail = match refusal {
                Refused::Unconfigured => "There is no machine yet.",
                Refused::Rom => "A ROM image does not fit the machine.",
                Refused::Disc => "It is not a disc image the machine reads.",
                Refused::Tape => "It is not a tape image the machine reads.",
                Refused::Snapshot => "It is not a snapshot of this machine.",
                Refused::TooLarge => "It is too large.",
                Refused::Memory => "The machine ran out of memory.",
                Refused::Malformed => "The request was malformed.",
            };
            self.refuse(what, detail);
        }

        fn ran(&mut self, ran: &Ran<'_>) {
            if let Some(share) = &self.share {
                for row in 0..CANVAS_HEIGHT {
                    if !ran.row_changed(row) || !share.copy_out(row * CANVAS_WIDTH, &mut self.slots)
                    {
                        continue;
                    }
                    let dots = u32::try_from(row)
                        .ok()
                        .and_then(|row| self.picture.screen_mut().row_mut(row));
                    if let Some(dots) = dots {
                        expand(&self.palette, &self.slots, dots);
                    }
                }
            }
            if !ran.sound.is_empty() {
                let volume = self.preferences.live().volume;
                let played = self
                    .sound
                    .as_mut()
                    .map(|sound| sound.play(ran.sound, volume, &mut self.scratch));
                if let Some(Err(err)) = played {
                    self.close_sound();
                    self.say(
                        format!("The sound stream failed ({err}); the machine is silent."),
                        StatusTone::Warning,
                    );
                }
            }
            let filed = self.shelf.filed();
            let settled = PhysicalDrive::ALL.iter().any(|drive| {
                let index = drive.index();
                self.drives[index] && !ran.drives[index] && filed[index]
            });
            if ran.lights != self.lights || ran.drives != self.drives {
                self.lights = ran.lights;
                self.drives = ran.drives;
                self.owe_strip();
            }
            self.loaded = ran.loaded;
            if self
                .running_from
                .is_some_and(|from| self.timeline >= from + CLOCK)
            {
                self.running_from = None;
                record(events::MACHINE_RUNNING, events::MACHINE_RUNNING_MESSAGE);
            }
            if settled {
                self.ask(Pending::CollectDiscs);
            }
        }

        /// The media a request took out or collected, through the shelf: the
        /// files it says to write and let go, the saves it offers.
        fn media(&mut self, media: &Media<'_>, asked: Option<Pending>) {
            for written in &media.discs {
                if let Some((side, track)) = written.lost {
                    self.say(
                        format!(
                            "Drive {}'s disc holds side {side} track {track} in a form its image cannot record; it is saved without it.",
                            written.drive.index()
                        ),
                        StatusTone::Warning,
                    );
                }
            }
            let restored = matches!(asked, Some(Pending::RestoreSnapshot(_)));
            let handover = match asked {
                Some(Pending::EjectDisc(drive)) => Handover::EjectDisc(drive),
                Some(Pending::InsertDisc { drive, disc, .. }) => Handover::InsertDisc(drive, disc),
                Some(Pending::EjectTape) => Handover::EjectTape,
                Some(Pending::InsertTape(tape)) => Handover::InsertTape(tape),
                Some(Pending::RestoreSnapshot(_)) => Handover::Restore,
                _ => Handover::Collect,
            };
            let mut work = Vec::new();
            self.shelf.handed(media, handover, &mut work);
            for job in work {
                match job {
                    FileWork::WriteBack { medium, bytes } => self.write_back(medium, bytes),
                    FileWork::Forget(medium) => self.forget(Some(medium)),
                }
            }
            if restored {
                self.owe_whole();
                self.say(
                    "The snapshot is restored; its discs and tape are its own.",
                    StatusTone::Ink,
                );
            }
            self.next_save();
        }

        /// The file a medium came from let go, once what was asked of it is done.
        fn forget(&mut self, medium: Option<u32>) {
            if let Some(medium) = medium {
                let _ = self.files.submit(Job::Forget(medium));
            }
        }

        fn write_back(&mut self, medium: u32, bytes: Vec<u8>) {
            self.submit(
                Job::WriteBack { medium, bytes },
                "A disc or tape could not be written back",
            );
        }

        fn file_done(&mut self, done: Done) {
            match done {
                Done::Roms(Ok((images, preferences))) => {
                    self.images = Some(images);
                    self.built = Some(preferences);
                    self.ask(Pending::Configure);
                }
                Done::Roms(Err(why)) => self.refuse("The machine could not be built", &why),
                Done::Taken {
                    purpose,
                    name,
                    bytes,
                    medium,
                    writable,
                } => self.taken(purpose, name, bytes, (medium, writable)),
                Done::Installed { socket, blob } => {
                    let was = self.preferences.live().clone();
                    let mut now = was.clone();
                    match socket {
                        Socket::Os => now.os = Image::Installed(blob),
                        Socket::Slot(slot) => {
                            if let Some(fitted) = now.slots.get_mut(usize::from(slot)) {
                                *fitted = Image::Installed(blob);
                            }
                        }
                    }
                    self.edited(&Edit {
                        was,
                        now,
                        settled: true,
                    });
                    self.say(
                        "The ROM image is installed; it is fitted at the next reset.",
                        StatusTone::Success,
                    );
                }
                Done::Written(Ok(())) | Done::Nothing | Done::Sound(None) => {}
                Done::Written(Err(why)) => self.refuse("A file could not be saved", &why),
                Done::Sound(Some(mut stream)) => {
                    let port = stream.stream.notify_port();
                    let watched = tairix_rt::waitset_ctl(
                        self.set,
                        WaitSetOp::Add,
                        WaitSourceKind::Port,
                        port,
                        SOUND,
                    );
                    if watched == 0 {
                        self.sound = Some(stream);
                    } else {
                        let _ = stream.stream.close(&mut stream.audio);
                        self.say(
                            "The sound stream cannot be waited on; the machine is silent.",
                            StatusTone::Warning,
                        );
                    }
                }
                Done::Failed(why) => self.refuse("A file could not be used", &why),
            }
        }

        fn taken(
            &mut self,
            purpose: Purpose,
            name: String,
            bytes: Vec<u8>,
            (medium, writable): (Option<u32>, bool),
        ) {
            match purpose {
                Purpose::Disc(drive) => {
                    let format = media::format_of(&name);
                    let disc = Disc {
                        name,
                        format,
                        medium,
                        image: bytes,
                    };
                    self.ask(Pending::InsertDisc {
                        drive,
                        write_protected: !writable,
                        disc,
                    });
                }
                Purpose::Tape => self.ask(Pending::InsertTape(Tape {
                    name,
                    medium,
                    image: bytes,
                })),
                Purpose::Snapshot => self.ask(Pending::RestoreSnapshot(bytes)),
                Purpose::Rom(_) => {}
            }
        }

        fn pick_open(&mut self, purpose: Purpose) {
            if !self.shelf.choose_to_open(purpose) {
                self.say(
                    "Another file is being chosen; finish with it first.",
                    StatusTone::Warning,
                );
                return;
            }
            let opened = self.window_id().map_or(Err(Errno::NotFound), |window_id| {
                self.client.pick_file(window_id, PickPurpose::Open)
            });
            if let Err(err) = opened {
                self.shelf.chosen();
                self.refuse("The file picker could not be opened", &format!("{err}"));
                self.next_save();
            }
        }

        fn offer_save(&mut self, saving: Saving) {
            self.shelf.offer(saving);
            self.next_save();
        }

        /// The next save offered, once no picker is open.
        fn next_save(&mut self) {
            while let Some((suggested, holding)) = self.shelf.choose_to_save() {
                let opened = match (self.window_id(), DocumentName::new(&suggested)) {
                    (Some(window_id), Ok(suggested)) => self
                        .client
                        .pick_file(
                            window_id,
                            PickPurpose::Save {
                                suggested,
                                formats: save_formats(holding),
                            },
                        )
                        .map_err(|err| format!("{err}")),
                    _ => Err(String::from("Its name could not be offered.")),
                };
                match opened {
                    Ok(()) => return,
                    Err(why) => {
                        self.shelf.chosen();
                        self.refuse("It could not be saved", &why);
                    }
                }
            }
        }

        /// A file picked: its name asked for, the grant held until it lands.
        fn picked(&mut self, window_id: u64, handle: u64, writable: bool) {
            if let Some((stale, _)) = self.picked.replace((handle, writable)) {
                drop(File::from_delegation(stale));
            }
            if let Err(err) = self.client.ask_picked_name(window_id) {
                self.picked = None;
                drop(File::from_delegation(handle));
                self.shelf.chosen();
                self.refuse("The picked file could not be named", &format!("{err}"));
                self.next_save();
            }
        }

        /// The picker closed with nothing picked.
        fn pick_cancelled(&mut self) {
            if let Some((grant, _)) = self.picked.take() {
                drop(File::from_delegation(grant));
            }
            self.shelf.chosen();
            self.next_save();
        }

        fn named(&mut self, name: Result<String, Errno>) {
            let Some((grant, writable)) = self.picked.take() else {
                return;
            };
            let job = match self.shelf.chosen() {
                Some(Chosen::Open(purpose)) => Some(Job::Take {
                    grant,
                    writable,
                    name: name.unwrap_or_else(|_| String::from("Untitled")),
                    purpose,
                }),
                Some(Chosen::Save(bytes)) => Some(Job::SaveTo { grant, bytes }),
                None => {
                    drop(File::from_delegation(grant));
                    None
                }
            };
            if let Some(job) = job {
                self.submit(job, "The file could not be used");
            }
            self.next_save();
        }

        fn opened(&mut self, target: Opened) {
            let (name, grant, writable) = match target {
                Opened::Document {
                    name,
                    grant,
                    writable,
                } => (name, grant, writable),
                Opened::Placed { name, grant, .. } => (name, grant, false),
                Opened::Path(_) | Opened::Image(_) | Opened::Pane(_) => return,
            };
            let Some(purpose) = Purpose::of(&name) else {
                drop(File::from_delegation(grant));
                self.refuse(
                    "The file could not be used",
                    &format!("{name} is not a disc or tape image."),
                );
                return;
            };
            let job = Job::Take {
                grant,
                writable,
                name,
                purpose,
            };
            self.submit(job, "The file could not be used");
        }

        /// The BBC's changes for a host key edge, each at a cycle the keyboard
        /// scan sees it: a key let go no sooner than held long enough.
        fn key(&mut self, input: &tairix_abi::input::KeyInput) {
            let mut presses = Vec::new();
            self.keyboard.edge(input, &mut presses);
            self.press(&presses);
        }

        fn press(&mut self, presses: &[Press]) {
            let last = self.inputs.back().map_or(self.timeline, |&(at, _)| at);
            let mut at = self.timeline.max(last);
            for &press in presses {
                let change = match press {
                    Press::Key(key, down) => {
                        let held = &mut self.pressed_at[usize::from(key.number() & 0x7F)];
                        if down {
                            *held = at;
                        } else {
                            at = at.max(*held + LEAST_HOLD);
                        }
                        Change::Key(key, down)
                    }
                    Press::Break(down) => Change::Break(down),
                };
                self.inputs.push_back((at, change));
            }
        }

        /// `text` typed key by key, SHIFT as each character needs.
        fn type_text(&mut self, text: &str) {
            let last = self.inputs.back().map_or(self.timeline, |&(at, _)| at);
            let mut at = self.timeline.max(last);
            for ch in text.chars().take(MOST_PASTE) {
                let Some((key, shifted)) = keys::typing(ch, self.lights.caps_lock) else {
                    continue;
                };
                let up = at + LEAST_HOLD;
                if shifted {
                    self.inputs.push_back((at, Change::Key(Key::SHIFT, true)));
                }
                self.inputs.push_back((at, Change::Key(key, true)));
                self.inputs.push_back((up, Change::Key(key, false)));
                if shifted {
                    self.inputs.push_back((up, Change::Key(Key::SHIFT, false)));
                }
                at += TYPE_GAP;
            }
        }

        fn command(&mut self, command: Command) {
            match command {
                Command::InsertDisc(drive) => self.pick_open(Purpose::Disc(drive)),
                Command::EjectDisc(drive) => self.ask(Pending::EjectDisc(drive)),
                Command::SaveDisc(drive) => {
                    if let Some(disc) = self.shelf.disc(drive) {
                        let saving = Saving {
                            suggested: disc.name.clone(),
                            holding: Holding::Disc,
                            bytes: disc.image.clone(),
                        };
                        self.offer_save(saving);
                    }
                }
                Command::InsertTape => self.pick_open(Purpose::Tape),
                Command::RewindTape => self.ask(Pending::RewindTape),
                Command::Record => self.ask(Pending::RecordTape(!self.loaded.recording)),
                Command::EjectTape => self.ask(Pending::EjectTape),
                Command::Break => self.press(&[Press::Break(true), Press::Break(false)]),
                Command::Reset => {
                    let rebuild = self
                        .built
                        .as_ref()
                        .is_none_or(|built| built.builds_other_than(self.preferences.live()));
                    if rebuild {
                        self.build();
                    } else {
                        self.ask(Pending::PowerOn);
                    }
                }
                Command::Pause => {
                    let paused = !self.pacer.paused();
                    self.pacer.set_paused(paused);
                    if paused {
                        let mut out = Vec::new();
                        self.keyboard.release_all(&mut out);
                        self.press(&out);
                    }
                    self.owe_strip();
                }
                Command::Speed(speed) => {
                    self.pacer.set_speed(speed);
                    self.owe_strip();
                }
                Command::FullScreen => {
                    if let Some(window_id) = self.window_id() {
                        let state = if self.full_screen {
                            WindowSizeState::Restored
                        } else {
                            WindowSizeState::Fullscreen
                        };
                        if let Err(err) = self.client.set_size_state(window_id, state) {
                            self.say(
                                format!("The window could not fill the screen ({err})."),
                                StatusTone::Warning,
                            );
                        }
                    }
                }
                Command::Scaling(scaling) => self.set(|record| record.scaling = scaling),
                Command::ScanLines => self.set(|record| record.scan_lines = !record.scan_lines),
                Command::Keyboard(mapping) => self.set(|record| record.keyboard = mapping),
                Command::Paste => {
                    if let Some(window_id) = self.window_id() {
                        if let Err(err) = self.client.ask_clipboard(window_id) {
                            self.say(
                                format!("The clipboard could not be read ({err})."),
                                StatusTone::Warning,
                            );
                        }
                    }
                }
                Command::SaveSnapshot => self.ask(Pending::TakeSnapshot),
                Command::RestoreSnapshot => self.pick_open(Purpose::Snapshot),
                Command::Settings => self.open_sheet(),
            }
        }

        /// A setting changed from the menu, written as a settled edit.
        fn set(&mut self, change: impl FnOnce(&mut Preferences)) {
            let was = self.preferences.live().clone();
            let mut now = was.clone();
            change(&mut now);
            self.edited(&Edit {
                was,
                now,
                settled: true,
            });
        }

        /// An edit carried into the live settings and what they show, and,
        /// settled, written.
        fn edited(&mut self, edit: &Edit<Preferences>) {
            self.preferences.edit(&edit.was, &edit.now);
            self.adopt_live();
            if edit.settled {
                let job = self.preferences.settle();
                tairix_appdata::submit_published(
                    &*self.publisher,
                    &mut self.preferences,
                    job,
                    &mut self.refusals,
                );
                self.state_refusals();
            }
        }

        /// The live settings shown: the keyboard, the picture and the
        /// settings window.
        fn adopt_live(&mut self) {
            let live = self.preferences.live().clone();
            let mut out = Vec::new();
            self.keyboard.set_mapping(live.keyboard, &mut out);
            self.press(&out);
            if self.picture.screen().scaling() != live.scaling {
                self.picture.screen_mut().set_scaling(live.scaling);
                self.owe_whole();
            }
            let tube = if live.scan_lines {
                SCAN_LINES
            } else {
                Tube::default()
            };
            if self.picture.tube() != tube {
                self.picture.set_tube(tube);
                self.owe_whole();
            }
            if let Some(window) = self.sheet.as_mut() {
                let mut damage = Region::new();
                if window.sheet.adopt(&live, &mut damage) {
                    sheet_present(
                        window,
                        &mut self.client,
                        self.themes.active(),
                        self.desktop.scale(),
                        &damage,
                    );
                }
            }
        }

        fn state_refusals(&mut self) {
            for refusal in core::mem::take(&mut self.refusals) {
                self.say(format!("{refusal}"), StatusTone::Warning);
            }
        }

        fn open_sheet(&mut self) {
            if self.sheet.is_some() {
                return;
            }
            let sheet = Sheet::new(self.preferences.live().clone(), 0);
            let (width, height) = sheet.size();
            let (width, height) = self.desktop.window_size(width, height);
            let mode = app::mode_for(width, height);
            let Some(surface) = Surface::new(mode.width_px, mode.height_px) else {
                self.refuse(
                    "The settings could not be opened",
                    "There is not enough memory.",
                );
                return;
            };
            let least = sheet.min_size(self.themes.active(), self.desktop.scale(), ());
            let sizing = WindowSizing::Resizable {
                min_width_px: least.0,
                min_height_px: least.1,
                max_width_px: 0,
                max_height_px: 0,
            };
            match WindowPane::open(
                &mut self.client,
                self.endpoint,
                &mode,
                sheet.title(),
                sizing,
            ) {
                Ok(pane) => {
                    let mut window = SheetWindow {
                        pane,
                        surface,
                        sheet,
                    };
                    let mut damage = Region::new();
                    window.sheet.lay_out(
                        mode.width_px,
                        mode.height_px,
                        self.themes.active(),
                        self.desktop.scale(),
                        (),
                        &mut damage,
                    );
                    damage.add(Rect::new(0, 0, mode.width_px, mode.height_px));
                    sheet_present(
                        &mut window,
                        &mut self.client,
                        self.themes.active(),
                        self.desktop.scale(),
                        &damage,
                    );
                    self.sheet = Some(window);
                }
                Err(err) => self.refuse("The settings could not be opened", &format!("{err}")),
            }
        }

        fn close_sheet(&mut self) {
            if let Some(window) = self.sheet.take() {
                let _ = window.pane.close(&mut self.client);
            }
        }

        fn sheet_event(&mut self, event: &WindowEvent, now: u64) {
            let Some(window) = self.sheet.as_mut() else {
                return;
            };
            let (theme, scale) = (self.themes.active(), self.desktop.scale());
            let mut damage = Region::new();
            let mut requests = Vec::new();
            match *event {
                WindowEvent::Key { key, .. } => {
                    requests.push(window.sheet.input(
                        &key_input_event(key),
                        now,
                        scale,
                        theme,
                        &mut damage,
                    ));
                }
                WindowEvent::Pointer { x, y, action, .. } => {
                    for input in pointer_input_events(action, pointer_point(x, y)) {
                        requests.push(window.sheet.input(&input, now, scale, theme, &mut damage));
                    }
                }
                WindowEvent::Resized {
                    width_px,
                    height_px,
                    ..
                } => {
                    let mode = app::mode_for(width_px, height_px);
                    if window
                        .pane
                        .resize_with(&mut self.client, &mode, &mut window.surface)
                        == Fitted::Adopted
                    {
                        window
                            .sheet
                            .lay_out(width_px, height_px, theme, scale, (), &mut damage);
                        damage.add(Rect::new(0, 0, width_px, height_px));
                    }
                }
                WindowEvent::CloseRequested { .. } => {
                    self.close_sheet();
                    return;
                }
                WindowEvent::RedrawRequested { .. } | WindowEvent::ContentReleased { .. } => {
                    let mode = *window.pane.mode();
                    damage.add(Rect::new(0, 0, mode.width_px, mode.height_px));
                }
                _ => {}
            }
            sheet_present(window, &mut self.client, theme, scale, &damage);
            for outcome in requests {
                match outcome.request {
                    Some(AppRequest::Close) => self.close_sheet(),
                    Some(AppRequest::Own(request)) => self.sheet_request(request),
                    None => {}
                }
            }
        }

        fn sheet_request(&mut self, request: SettingsRequest<Preferences>) {
            if let Some(edit) = request.edit {
                self.edited(&edit);
            }
            match request.then {
                Some(Then::Restore) => self.set(|record| *record = Preferences::default()),
                Some(Then::Ask(socket)) => self.pick_open(Purpose::Rom(socket)),
                None => {}
            }
        }

        fn window_event(&mut self, event: WindowEvent, now: u64) {
            let ours = self.window_id();
            let sheet = self.sheet.as_ref().map(|window| window.pane.id());
            if event.window_id().is_some() && event.window_id() == sheet {
                self.sheet_event(&event, now);
                return;
            }
            match event {
                WindowEvent::Key { key, .. } => self.key(&key),
                WindowEvent::Focus { focused: false, .. } => {
                    let mut out = Vec::new();
                    self.keyboard.release_all(&mut out);
                    self.press(&out);
                }
                WindowEvent::Pointer {
                    window_id,
                    x,
                    y,
                    action: PointerAction::Pressed(PointerButtonCode::Secondary),
                    ..
                } if Some(window_id) == ours => self.open_menu(window_id, (x, y)),
                WindowEvent::MenuClosed {
                    open_id, outcome, ..
                } => {
                    if self.menu_open.take_if(|open| *open == open_id).is_some() {
                        if let MenuOutcome::Chosen(item) = outcome {
                            if let Some(command) = menu::command(item) {
                                self.command(command);
                            }
                        }
                    }
                }
                WindowEvent::AppBarMenu { item, .. } => {
                    if is_quit(item) {
                        self.quit();
                    } else if let Some(command) = menu::command(item) {
                        self.command(command);
                    }
                }
                WindowEvent::Resized {
                    width_px,
                    height_px,
                    state,
                    ..
                } => {
                    self.full_screen = state.is_fullscreen();
                    let mode = app::mode_for(width_px, height_px);
                    if let Some(pane) = self.pane.as_mut() {
                        if pane.resize_with(&mut self.client, &mode, &mut self.surface)
                            == Fitted::Kept
                        {
                            app::report(APP_NAME, app::SIZE_KEPT);
                        }
                    }
                    self.owe_whole();
                }
                WindowEvent::RedrawRequested { .. } => self.owe_whole(),
                WindowEvent::ContentReleased { .. } => {
                    if let Some(pane) = self.pane.as_mut() {
                        pane.release_frames();
                    }
                    self.owe_whole();
                }
                WindowEvent::CloseRequested { .. } => self.quit(),
                WindowEvent::FilePicked {
                    window_id,
                    handle,
                    writable,
                } => self.picked(window_id, handle, writable),
                WindowEvent::PickCancelled { .. } => self.pick_cancelled(),
                WindowEvent::FileOffered {
                    window_id, serial, ..
                } => {
                    if let Err(err) = self.client.ask_offered_file(window_id, serial) {
                        app::report(
                            APP_NAME,
                            format_args!("a dropped file could not be judged ({err})"),
                        );
                    }
                }
                WindowEvent::OpenRequested => {
                    if let Err(err) = self.client.ask_open_targets() {
                        app::report(
                            APP_NAME,
                            format_args!("the files to open could not be asked for ({err})"),
                        );
                    }
                }
                _ => {}
            }
        }

        /// Serve what woke the loop: the desktop, the files, the settings, the
        /// sound and the machine's session.
        fn woken(&mut self, fired: Woke, now: u64) {
            if fired.has(Woke::DESKTOP)
                && app::adopt_desktop(&mut self.desktop, &mut self.themes).unwrap_or(false)
            {
                self.owe_whole();
            }
            if fired.has(Woke::FILES) {
                let files = Arc::clone(&self.files);
                files.collect_landed(|done| self.file_done(done));
            }
            if fired.has(Woke::SETTINGS)
                && tairix_appdata::collect_published(
                    &*self.publisher,
                    &mut self.preferences,
                    &mut self.refusals,
                )
            {
                self.state_refusals();
                self.adopt_live();
            }
            if fired.has(Woke::SOUND) {
                if let Some(sound) = self.sound.as_mut() {
                    sound.drain();
                }
            }
            self.serve_session(now, (fired.has(Woke::READABLE), fired.has(Woke::WRITABLE)));
            self.restart_due(now);
            if self.picture.step(now) {
                self.owe_picture();
            }
        }

        /// The disc or tape the application was opened with.
        fn launched_document(&mut self) {
            let writable = match tairix_rt::arg(1) {
                Some(arg) if arg == DOCUMENT_WRITABLE_ROLE_ARG => true,
                Some(arg) if arg == DOCUMENT_ROLE_ARG => false,
                _ => return,
            };
            let name = tairix_rt::arg(2)
                .and_then(|raw| core::str::from_utf8(raw).ok())
                .map(|path| String::from(path.rsplit('/').next().unwrap_or(path)))
                .unwrap_or_default();
            let job = Job::Launched {
                file: Some(File::adopt(STDIN)),
                writable,
                name,
            };
            self.submit(job, "The file could not be used");
        }

        fn answer(&mut self, answer: Answer) {
            match answer {
                Answer::FrameReturned { window_id } => {
                    if let Some(pane) = self.pane.as_mut().filter(|pane| pane.id() == window_id) {
                        let _ = pane.present_deferred(&mut self.client, &self.surface);
                    } else if let Some(window) = self
                        .sheet
                        .as_mut()
                        .filter(|window| window.pane.id() == window_id)
                    {
                        let _ = window
                            .pane
                            .present_deferred(&mut self.client, &window.surface);
                    }
                }
                Answer::PickedName { name, .. } => self.named(name),
                Answer::OfferedFile(offer) => {
                    let ours = offer
                        .media
                        .as_deref()
                        .ok()
                        .and_then(MediaType::from_media_str)
                        .is_some_and(|media| {
                            matches!(
                                media,
                                MediaType::AcornDfsDisc | MediaType::TapeUef | MediaType::TapeCsw
                            )
                        });
                    let _ = self
                        .client
                        .answer_file_offer(offer.window_id, offer.serial, ours);
                }
                Answer::OpenTarget(Ok(Some(target))) => self.opened(target),
                Answer::OpenTarget(Err(err)) => {
                    app::report(APP_NAME, format_args!("a file to open was lost ({err})"));
                }
                Answer::Clipboard { contents, .. } => match contents {
                    Ok(Some((ClipboardKind::Text, bytes))) => {
                        let text = String::from_utf8_lossy(&bytes);
                        self.type_text(&text);
                    }
                    Ok(_) => self.say("The clipboard holds no text.", StatusTone::Muted),
                    Err(err) => self.say(
                        format!("The clipboard could not be read ({err})."),
                        StatusTone::Warning,
                    ),
                },
                _ => {}
            }
        }

        fn shown(&self) -> Shown {
            let live = self.preferences.live();
            Shown {
                discs: self.loaded.discs,
                images: PhysicalDrive::ALL.map(|drive| self.shelf.disc(drive).is_some()),
                tape: self.loaded.tape,
                recording: self.loaded.recording,
                paused: self.pacer.paused(),
                speed: self.pacer.speed(),
                full_screen: self.full_screen,
                scaling: live.scaling,
                scan_lines: live.scan_lines,
                keyboard: live.keyboard,
            }
        }

        fn open_menu(&mut self, window_id: u64, (x, y): (u32, u32)) {
            let mut rows = AppMenu::EMPTY;
            let built = menu::push_rows(&mut rows, &self.shown());
            let at = |n: u32| i32::try_from(n).map_err(|_| Errno::OutOfRange);
            let opened = built.and_then(|()| {
                let anchor = WindowRegion::new(at(x)?, at(y)?, 0, 0)?;
                self.client.open_menu(window_id, anchor, &rows)
            });
            match opened {
                Ok(open) => self.menu_open = Some(open),
                Err(err) => app::report(
                    APP_NAME,
                    format_args!("the menu could not be opened ({err})"),
                ),
            }
        }

        /// The icon bar's menu, declared again whenever what it shows changes.
        fn declare_bar(&mut self) {
            let shown = self.shown();
            if self.shown == Some(shown) {
                return;
            }
            self.shown = Some(shown);
            self.bar_revision = self.bar_revision.wrapping_add(1);
            let declared = menu::bar(&shown).and_then(|menu| {
                let bar = AppBar {
                    event_endpoint: self.endpoint,
                    click: AppBarClick::Raise,
                    menu,
                    revision: self.bar_revision,
                };
                self.client.set_app_bar(&bar)
            });
            if let Err(err) = declared {
                app::report(
                    APP_NAME,
                    format_args!("the icon bar's menu could not be declared ({err})"),
                );
            }
        }

        /// Every medium taken out, so what the machine alone holds is saved
        /// or offered to be, before the application ends.
        fn quit(&mut self) {
            if self.quitting {
                return;
            }
            self.quitting = true;
            self.pending.clear();
            if self.session.is_live() && self.images.is_some() {
                for drive in PhysicalDrive::ALL {
                    self.ask(Pending::EjectDisc(drive));
                }
                self.ask(Pending::EjectTape);
            }
        }

        /// Whether the work quitting waits on is done.
        fn quit_done(&self) -> bool {
            self.quitting
                && self.pending.is_empty()
                && (self.machine.is_idle() || !self.session.is_live())
                && self.shelf.is_settled()
        }

        fn close_sound(&mut self) {
            if let Some(mut stream) = self.sound.take() {
                let port = stream.stream.notify_port();
                let _ = tairix_rt::waitset_ctl(
                    self.set,
                    WaitSetOp::Del,
                    WaitSourceKind::Port,
                    port,
                    SOUND,
                );
                let _ = stream.stream.close(&mut stream.audio);
            }
        }

        /// Draw what changed into the surface, then present only that.
        fn paint(&mut self, now: u64) {
            let (Some((target, strip)), Some(mode)) = (self.layout(), self.mode()) else {
                return;
            };
            let drawing = Drawing {
                percent: self.desktop.scale().percent(),
                runner: self.pool.runner(),
                refresh: self.desktop.refresh(),
                now_ns: now,
            };
            let mut damage = core::mem::take(&mut self.damage);
            if damage.bounds().height >= mode.height_px {
                self.surface.fill(Color::rgb(0, 0, 0));
            }
            match self.picture.draw(&mut self.surface, target, drawing) {
                Ok(drawn) => damage.add_region(&drawn),
                Err(why) => app::report(
                    APP_NAME,
                    format_args!("the picture could not be drawn ({why:?})"),
                ),
            }
            if damage.intersects(strip) {
                let parts =
                    strip_parts(self.lights, self.drives, self.message.as_ref(), &self.pacer);
                paint_strip(
                    &mut self.surface,
                    strip,
                    self.themes.active(),
                    self.desktop.scale(),
                    &parts,
                );
            }
            self.owed.owe(&mode, Repaint::Reported, &damage);
            let (Some(pane), Some(parts)) = (self.pane.as_mut(), self.owed.take(&mode)) else {
                return;
            };
            if let Err(err) = pane.present_list(&mut self.client, &self.surface, &parts) {
                app::report(
                    APP_NAME,
                    format_args!("the window could not be presented ({err})"),
                );
            }
        }

        fn due(&mut self, now: u64) -> Option<u64> {
            let mut due = self.session.restart_deadline();
            let mut earliest = |at: Option<u64>| {
                due = match (due, at) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
            };
            earliest(self.picture.due());
            if self.machine.is_idle()
                && self.pending.is_empty()
                && self.images.is_some()
                && !self.quitting
            {
                let hurried = self.preferences.live().fast_tape && self.lights.motor;
                let ring = self.sound.as_mut().and_then(Speaker::ring);
                let mut pacer = self.pacer;
                if let Next::At(at) = pacer.next(now, ring, hurried) {
                    earliest(Some(at));
                }
            }
            if let Some(window) = self.sheet.as_mut() {
                earliest(window.sheet.deadline_ns(now));
            }
            due
        }
    }

    /// `pending` as the request it is, borrowing what it carries, a collect
    /// reaching the drives `collect` marks; `None` for one that asks nothing:
    /// a build whose images are not read, a collect marking no drive.
    fn request_of<'a>(
        pending: &'a Pending,
        images: Option<&'a Images>,
        links: u8,
        collect: [bool; 2],
    ) -> Option<Request<'a>> {
        Some(match pending {
            Pending::Configure => Request::Configure(Box::new(images?.configuration(links))),
            Pending::InsertDisc {
                drive,
                write_protected,
                disc,
            } => Request::InsertDisc {
                drive: *drive,
                format: disc.format,
                write_protected: *write_protected,
                image: &disc.image,
            },
            Pending::EjectDisc(drive) => Request::EjectDisc(*drive),
            Pending::InsertTape(tape) => Request::InsertTape(&tape.image),
            Pending::EjectTape => Request::EjectTape,
            Pending::RewindTape => Request::RewindTape,
            Pending::RecordTape(on) => Request::RecordTape(*on),
            Pending::PowerOn => Request::PowerOn,
            Pending::TakeSnapshot => Request::TakeSnapshot,
            Pending::RestoreSnapshot(snapshot) => Request::RestoreSnapshot(snapshot),
            Pending::CollectDiscs if collect.contains(&true) => Request::CollectDiscs(collect),
            Pending::CollectDiscs => return None,
        })
    }

    /// The formats a save's picker offers for what it holds.
    fn save_formats(holding: Holding) -> SaveFormats {
        let mut formats = SaveFormats::ANY;
        let own = match holding {
            Holding::Disc => MediaType::AcornDfsDisc,
            Holding::Tape => MediaType::TapeUef,
            Holding::Snapshot => return formats,
        };
        let _ = formats.push(save_format(own), true);
        formats
    }

    fn sheet_present(
        window: &mut SheetWindow,
        client: &mut tairix_window::WindowClient<app::RtWindowTransport>,
        theme: &Theme,
        scale: tairix_geometry::Scale,
        damage: &Region,
    ) {
        if damage.is_empty() {
            return;
        }
        window
            .sheet
            .render(&mut window.surface, (theme, scale), &mut NoArtwork);
        let mode = *window.pane.mode();
        let damage = tairix_window::damage_in(&mode, damage.bounds())
            .unwrap_or_else(|| DamageRect::full(&mode));
        if let Err(err) = window.pane.present(client, &window.surface, damage) {
            app::report(
                APP_NAME,
                format_args!("the settings could not be presented ({err})"),
            );
        }
    }

    /// The status strip's texts: each lamp's label, and the message.
    fn strip_parts(
        lights: Lights,
        drives: [bool; 2],
        message: Option<&(String, StatusTone)>,
        pacer: &Pacer,
    ) -> ([(&'static str, bool); 5], (String, StatusTone)) {
        let lamps = [
            ("CAPS LOCK", lights.caps_lock),
            ("SHIFT LOCK", lights.shift_lock),
            ("MOTOR", lights.motor),
            ("DRIVE 0", drives[0]),
            ("DRIVE 1", drives[1]),
        ];
        let message = match (pacer.paused(), message) {
            (true, _) => (String::from("Paused"), StatusTone::Muted),
            (false, Some((text, tone))) => (text.clone(), *tone),
            (false, None) => (String::new(), StatusTone::Ink),
        };
        (lamps, message)
    }

    fn paint_strip(
        surface: &mut Surface,
        strip: Rect,
        theme: &Theme,
        scale: tairix_geometry::Scale,
        (lamps, message): &([(&'static str, bool); 5], (String, StatusTone)),
    ) {
        let band = StatusBand::new(theme, scale);
        let lamp = band.face().line_height() * 2 / 3;
        let widths = lamps.map(|(label, _)| band.slot_width(label) + lamp);
        let slots = band.slots::<5, 0>(strip, widths, []);
        let mut parts: Vec<(Rect, &str, StatusTone)> = slots
            .leading
            .iter()
            .zip(lamps.iter())
            .map(|(slot, (label, _))| {
                let text = Rect::new(
                    slot.left() + i32::try_from(lamp).unwrap_or(0),
                    slot.top(),
                    slot.width.saturating_sub(lamp),
                    slot.height,
                );
                (text, *label, StatusTone::Muted)
            })
            .collect();
        parts.push((slots.message, message.0.as_str(), message.1));
        band.paint(surface, strip, theme, &parts);
        let palette = theme.palette();
        let side = lamp.max(2);
        for (slot, (_, lit)) in slots.leading.iter().zip(lamps.iter()) {
            let colour = if *lit {
                palette.danger
            } else {
                palette.on_surface_muted
            };
            let (Ok(x), Ok(y)) = (
                u32::try_from(slot.left()),
                u32::try_from(
                    slot.top() + i32::try_from(slot.height.saturating_sub(side) / 2).unwrap_or(0),
                ),
            ) else {
                continue;
            };
            surface.fill_round_rect(x, y, side, side, side / 2, Color::from(colour));
        }
    }

    tairix_rt::entry!(main);

    fn main() -> i32 {
        if session_worker_role() {
            return match spawn_share().ok().and_then(MachineService::new) {
                Some(mut service) => serve_session_stdio(&mut service).exit_code(),
                None => 1,
            };
        }
        if tairix_rt::arg(1).is_some_and(|arg| matches!(arg, b"-h" | b"--help" | b"-?")) {
            return tairix_help::print_own_short_help(APP_NAME, None);
        }
        run()
    }

    fn run() -> i32 {
        let _ = tairix_rt::latency_watch(DEFAULT_FRAME_BUDGET_NS);
        let (mut front, server) = match start() {
            Ok(started) => started,
            Err(code) => return code,
        };
        let files = Arc::clone(&front.files);
        let publisher = Arc::clone(&front.publisher);
        let _files = WorkerGuard::new(&files);
        let _settings = WorkerGuard::new(&publisher);
        let due = Cell::new(None);
        let woke = Cell::new(Woke::default());
        let events = WindowEvents::new(RtEventSource {
            mailbox: EventMailbox::new(front.endpoint, server),
            set: front.set,
            files: &files,
            settings: &publisher,
            due: &due,
            woke: &woke,
        });
        let code = serve(&mut front, events, (&due, &woke));
        front.close_sheet();
        front.close_sound();
        if let Some(pane) = front.pane.take() {
            let _ = pane.close(&mut front.client);
        }
        while let Some(done) = files.wait() {
            front.file_done(done);
        }
        tairix_appdata::see_out_published(&*publisher, &mut front.preferences, &mut front.refusals);
        front.state_refusals();
        code
    }

    /// The window opened, the workers started and the machine asked for.
    fn start() -> Result<(Front, ProcId), i32> {
        let mut client = WindowClient::new(app::RtWindowTransport);
        let (desktop, themes) = app::bring_up_desktop(&mut client)
            .map_err(|err| app::fail(APP_NAME, err.code(), err))?;
        let binding =
            app::bind_event_mailbox().map_err(|err| app::fail(APP_NAME, err.code(), err))?;
        let (endpoint, set) = (binding.endpoint(), binding.set());
        let (preferences, refusals) =
            tairix_appdata::loaded::<Preferences>(&Settings::open_without_defaults(&mut RtHost));
        let (publisher, files) = workers(set)?;
        let (session, share) = machine_session()?;
        let (pane, surface, server) = open_window(&mut client, (&desktop, &themes), endpoint)?;
        let picture = picture_of(&preferences)?;
        let online = tairix_procinfo::online_cpus(&tairix_procinfo::IpcTransport);
        let mut front = Front {
            client,
            pane: Some(pane),
            surface,
            owed: Owed::new(),
            endpoint,
            set,
            desktop,
            themes,
            session,
            members: SessionMembers::new(set, SESSION_READ, SESSION_WRITE),
            machine: MachineClient::new(),
            share,
            asked: None,
            pending: VecDeque::new(),
            inputs: VecDeque::new(),
            timeline: 0,
            pressed_at: [0; 0x80],
            pacer: Pacer::new(),
            keyboard: Keyboard::new(preferences.keyboard),
            images: None,
            built: None,
            preferences: Publication::new(preferences),
            refusals,
            publisher,
            files,
            sound: None,
            scratch: Vec::new(),
            shelf: Shelf::new(),
            picked: None,
            lights: Lights::default(),
            drives: [false; 2],
            loaded: Loaded::default(),
            picture,
            palette: palette(),
            slots: alloc::vec![0; CANVAS_WIDTH],
            pool: LazyPool::new(APP_NAME, online, "the picture"),
            menu_open: None,
            bar_revision: 0,
            shown: None,
            sheet: None,
            message: None,
            full_screen: false,
            damage: Region::new(),
            quitting: false,
            running_from: None,
        };
        front.state_refusals();
        front.build();
        let _ = front.files.submit(Job::Sound);
        front.launched_document();
        front.owe_whole();
        front.declare_bar();
        Ok((front, server))
    }

    /// The settings' writer and the file worker, started.
    fn workers(set: u64) -> Result<(Arc<PublishWorker<Preferences>>, Arc<Files>), i32> {
        let publisher = Arc::new(tairix_appdata::publish_worker::<Preferences>(Opening::Bare));
        app::start_worker(APP_NAME, &publisher, set, SETTINGS, "settings");
        let held = Held {
            next: 0,
            files: Vec::new(),
        };
        let files = Worker::queued(serve_files, held, WorkerWake::create(), FILE_JOBS)
            .map(Arc::new)
            .map_err(|_| {
                app::fail(
                    APP_NAME,
                    app::EXIT_NO_WINDOW,
                    "no memory for the file worker",
                )
            })?;
        app::start_worker(APP_NAME, &files, set, FILES, "files");
        Ok((publisher, files))
    }

    /// The machine's picture, shown as `preferences` say.
    fn picture_of(preferences: &Preferences) -> Result<Picture, i32> {
        let mut picture = Picture::new(if preferences.scan_lines {
            SCAN_LINES
        } else {
            Tube::default()
        });
        picture.screen_mut().set_scaling(preferences.scaling);
        let (Ok(across), Ok(down)) = (u32::try_from(CANVAS_WIDTH), u32::try_from(CANVAS_HEIGHT))
        else {
            return Err(app::fail(
                APP_NAME,
                app::EXIT_NO_WINDOW,
                "the canvas is too large",
            ));
        };
        if !picture.screen_mut().reshape(across, down) {
            return Err(app::fail(
                APP_NAME,
                app::EXIT_NO_WINDOW,
                "no memory for the picture",
            ));
        }
        Ok(picture)
    }

    /// The supervised session the machine runs in, its first worker started.
    fn machine_session() -> Result<(Session, Option<Arc<dyn Share>>), i32> {
        let share = NonZeroUsize::new(SHARE_BYTES)
            .ok_or_else(|| app::fail(APP_NAME, app::EXIT_NO_WINDOW, "the picture has no size"))?;
        let bounds = session_bounds()
            .map_err(|err| app::fail(APP_NAME, app::EXIT_NO_WINDOW, format_args!("{err:?}")))?;
        let launcher = RtSessionLauncher::own_binary(WorkerMemory::Documents).sharing(share);
        let mut session = SupervisedSession::new(launcher, bounds, LogSink);
        let _ = session.start(tairix_rt::clock_get());
        let share = session.share().cloned();
        Ok((session, share))
    }

    /// The machine's window, its keyboard read raw, and the desktop's
    /// session that will send its events.
    fn open_window(
        client: &mut WindowClient<app::RtWindowTransport>,
        (desktop, themes): (&Desktop, &ThemeRegistry),
        endpoint: u64,
    ) -> Result<(WindowPane, Surface, ProcId), i32> {
        let strip = StatusBand::new(themes.active(), desktop.scale()).height();
        let (width, height) = desktop.window_size(OPEN_PICTURE.0, OPEN_PICTURE.1);
        let least = desktop.window_size(LEAST_PICTURE.0, LEAST_PICTURE.1);
        let mode = app::mode_for(width, height.saturating_add(strip));
        let sizing = WindowSizing::Resizable {
            min_width_px: least.0,
            min_height_px: least.1.saturating_add(strip),
            max_width_px: 0,
            max_height_px: 0,
        };
        let surface = Surface::new(mode.width_px, mode.height_px)
            .ok_or_else(|| app::fail(APP_NAME, app::EXIT_NO_WINDOW, "no memory for the window"))?;
        let pane = WindowPane::open(client, endpoint, &mode, TITLE, sizing)
            .map_err(|err| app::fail(APP_NAME, err.code(), err))?;
        if let Err(err) = client.set_keyboard_mode(pane.id(), KeyboardMode::Raw) {
            app::report(
                APP_NAME,
                format_args!("the keyboard cannot be read raw ({err}); keys type text"),
            );
        }
        let server = client.session().ok_or_else(|| {
            app::fail(
                APP_NAME,
                app::EXIT_CHANNEL_LOST,
                "the desktop did not identify itself",
            )
        })?;
        Ok((pane, surface, server))
    }

    /// The loop: what woke it served, the window's events taken, the machine
    /// asked for its next slice, and one paint.
    fn serve(
        front: &mut Front,
        mut events: WindowEvents<RtEventSource<'_>>,
        (due, woke): (&Cell<Option<u64>>, &Cell<Woke>),
    ) -> i32 {
        let mut waited = Ok(None);
        loop {
            let now = tairix_rt::clock_get();
            front.woken(woke.replace(Woke::default()), now);
            loop {
                match waited {
                    Ok(Some(event)) => front.window_event(event, now),
                    Ok(None) => break,
                    Err(EventError::Undecodable(_)) => {}
                    Err(EventError::Refused(refused)) => {
                        app::report(APP_NAME, refused);
                        if front
                            .window_id()
                            .is_some_and(|window| app::refused_create(&refused, window))
                        {
                            return app::fail(
                                APP_NAME,
                                app::EXIT_CHANNEL_LOST,
                                "the window was refused",
                            );
                        }
                    }
                    Err(EventError::Mailbox(_)) => {
                        return app::fail(APP_NAME, app::EXIT_CHANNEL_LOST, "event channel lost");
                    }
                }
                waited = events.try_wait(&mut front.client);
            }
            while let Some(answer) = front.client.take_answer() {
                front.answer(answer);
            }
            if front.quit_done() {
                return 0;
            }
            front.pump(now);
            let _ = send_waiting(&mut front.machine, &mut front.session, now);
            let synced = front.members.sync(
                front.session.descriptors(),
                front.session.wants_read(),
                front.session.wants_write(),
            );
            if let Err(err) = synced {
                app::report(
                    APP_NAME,
                    format_args!("the machine's session cannot be waited on ({err})"),
                );
            }
            front.declare_bar();
            front.paint(now);
            due.set(front.due(now));
            waited = events.wait(&mut front.client);
        }
    }
}

#[cfg(not(freestanding))]
fn main() {}

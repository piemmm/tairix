//! The `PCEmulator.app` bundle's `Run` entry point.
//!
//! # Three roles
//!
//! Started by the user it is the front-end: the library window, a window for
//! each running machine, the application's store and each machine's sound.
//! Re-entered through `SPAWN_SELF` in the session-worker role it is one
//! machine — processor, board, BIOS and DOS — in a canonical sandbox holding
//! nothing but its two pipes and the spawn share its frames are written into
//! (`plans/PCEMU.md` §3). Re-entered as the compiler, it is a one-shot
//! sandbox that compiles the blocks a machine grows hot on and stages their
//! code for the front-end to install in that machine's worker (PC18.4).
//!
//! # What runs where
//!
//! The engine and the application's pure parts (`tairix_pcemu::app`) are
//! host-tested; this file joins them to the live windows, store, streams and
//! sessions. Nothing that waits runs on the loop that owes the windows their
//! frames. The library worker reads and writes the library, makes, copies and
//! removes disks, keeps each machine's last screen, and opens a machine's
//! disks and sound and starts its worker; the disk worker serves every running
//! machine's sectors, so a long copy never stalls a running guest; the compile
//! worker has each machine's compiler compile the blocks it asks for and
//! installs their code. The loop asks, and collects what lands.
//!
//! On the host it is an inert stub so `cargo build --workspace`, clippy, and
//! fmt still cover the file.

#![cfg_attr(freestanding, no_std)]
#![cfg_attr(freestanding, no_main)]
#![deny(missing_docs)]

#[cfg(freestanding)]
mod program {
    extern crate alloc;

    use alloc::boxed::Box;
    use alloc::format;
    use alloc::string::String;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::cell::Cell;
    use core::num::NonZeroUsize;

    use tairix_abi::appdata_ipc::BlobMode;
    use tairix_abi::audio::{OpenParams, StreamRole, Tenancy};
    use tairix_abi::driver::audio::{ChannelMap, Frames, Rate, SampleFormat, StreamDirection};
    use tairix_abi::waitset::{WaitSetOp, WaitSourceKind};
    use tairix_abi::window_ipc::{
        AlertOutcome, AlertSeverity, AlertSpec, AppMenu, AppMenuItem, AppMenuItemId, AppMenuLabel,
        AppMenuRow, AppMenuSymbol, ClipboardKind, CursorShape, DialogModality, DocumentName,
        KeyboardMode, MenuOutcome, PickPurpose, PointerAction, SaveFormats, WindowEvent,
        WindowRegion, WindowSizeState,
    };
    use tairix_abi::{Errno, SPAWN_SELF};
    use tairix_appdata::open_blob;
    use tairix_audio::live::{LiveStream, RtAudio};
    use tairix_browse::media::save_format;
    use tairix_browse::MediaType;
    use tairix_crt::screen::{fit, screenshot, thumbnail, Drawing, Picture, Screen};
    use tairix_crt::Tube;
    use tairix_geometry::{Rect, Region, Scale};
    use tairix_input::InputEvent;
    use tairix_machines::assistant::{self as assistance, Assistant};
    use tairix_machines::keeping::{Ended, Suspends};
    use tairix_machines::menu::{card_command, card_menu, CardCommand, Followed};
    use tairix_machines::record::Kept;
    use tairix_machines::shelf::{Entry, LibraryLook, Pictures, Request, Shelf};
    use tairix_machines::state::{LastScreen, Suspended};
    use tairix_machines::store::{self as kept, Resumable, Shelved};
    use tairix_machines::window::{
        self as library_window, event_time, inputs, present, repaint, whole, BarAsk, LibraryEvent,
        LibraryWindow, SheetEvent, SheetWindow,
    };
    use tairix_machines::{Found, Library, Record, Standing, Writes};
    use tairix_pcemu::app::assistant;
    use tairix_pcemu::app::bar::{MachineBar, Menu, MenuRequest, Shared as ShareShown};
    use tairix_pcemu::app::debugger::{self as debugging, Added, Adding, Ask, Debugger};
    use tairix_pcemu::app::disk::{self, Medium};
    use tairix_pcemu::app::input::{ctrl_alt_delete, held_title, Asks, Input, Over};
    use tairix_pcemu::app::library::{self, Machine, StoredDisk, STATE_BOUNDS};
    use tairix_pcemu::app::media::{self, Unfit};
    use tairix_pcemu::app::menu::{self as machine_menu, Clipboard, Command, Shown, Windows};
    use tairix_pcemu::app::screen::{opening_extent, VGA_SCREEN};
    use tairix_pcemu::app::settings::{self, Settings};
    use tairix_pcemu::app::sound::{Sound, Stream, StreamError};
    use tairix_pcemu::app::text;
    use tairix_pcemu::cpu::jit::cache::Key;
    use tairix_pcemu::cpu::jit::compiler::Compile;
    use tairix_pcemu::cpu::jit::dispatch::Execute;
    use tairix_pcemu::cpu::jit::native::Native;
    use tairix_pcemu::dos::setup::default_disk;
    use tairix_pcemu::folder::server::Server;
    use tairix_pcemu::folder::{
        self, alias, DosName, Folder as _, Refusal as FolderRefusal, Reply, SHARES,
    };
    use tairix_pcemu::front::{Ending, Host, HostEvent, Refused, StartError};
    use tairix_pcemu::link::layout::Layout;
    use tairix_pcemu::link::{
        FixedDisk, Form, InsertRefusal, Installed, Setup, Text, Unkept, COMPILE_WINDOW,
    };
    use tairix_pcemu::machine::Template;
    use tairix_pcemu::roles::{self, HeldCompiler, Interpret};
    use tairix_pcemu::storage::hdc::Recording;
    use tairix_pcemu::storage::{MediaError, SECTOR};
    use tairix_pcemu::time::{CpuClock, NANOS_PER_SECOND};
    use tairix_pcemu::video::Unread;
    use tairix_raster::resample::Region as Target;
    use tairix_raster::{Color, Surface};
    use tairix_rt::sync::WorkerWake;
    use tairix_rt::work::{Worker, WorkerGuard};
    use tairix_rt::{File, LogSink};
    use tairix_sandbox::rt::{worker_ceiling, RtSessionChannel, SessionMembers};
    use tairix_sandbox::WorkerMemory;
    use tairix_theme::{Theme, ThemeRegistry};
    use tairix_util::defer::JobQueue;
    use tairix_window::app::{self, Fitted, LazyPool, Wake, WindowPane};
    use tairix_window::{
        pointer_input_events, pointer_point, Answer as Landed, Desktop, EventDrain, EventError,
        EventMailbox, EventSource, Owed, Parked, Repaint, WindowClient, WindowEvents, WindowSizing,
        EVENT_MAILBOX_CAPACITY,
    };

    /// The name this program states its own refusals under: its manifest's.
    const APP_NAME: &str = "PCEmulator";

    /// The library window's title.
    const APP_TITLE: &str = "PC Emulator";

    /// A diskette drive's menu rows, and a shared folder's.
    const INSERT_ROW: u16 = 1;
    const EJECT_ROW: u16 = 2;
    const STOP_SHARING_ROW: u16 = 1;

    /// Wait-set tokens: the library worker's answers, the disk worker's, the
    /// compile worker's, then each running machine's read pipe, write pipe
    /// and sound in turn.
    const STORE_TOKEN: u64 = app::FIRST_APP_TOKEN;
    const DISK_TOKEN: u64 = app::FIRST_APP_TOKEN + 1;
    const COMPILE_TOKEN: u64 = app::FIRST_APP_TOKEN + 2;
    const FIRST_MACHINE_TOKEN: u64 = app::FIRST_APP_TOKEN + 3;
    const TOKENS_PER_MACHINE: u64 = 3;

    // Every token's wake is a bit of one 64-bit word.
    const _: () = assert!(3 + TOKENS_PER_MACHINE * Machine::MAX_MACHINES as u64 <= 64);

    /// The least a machine window is made, in logical pixels: the monitor's
    /// shape at half a VGA screen. A machine window opens at its picture's own
    /// size ([`opening_extent`]).
    const LEAST_SIZE: (u32, u32) = (320, 240);
    /// The debugger window's size as it opens, and the least it is made.
    const DEBUGGER_SIZE: (u32, u32) = (1100, 760);
    const DEBUGGER_LEAST: (u32, u32) = (760, 520);

    /// Jobs each worker holds unanswered: every machine's few at once, and for
    /// the compile worker every machine's window of blocks and its end.
    const STORE_JOBS: usize = 4 * Machine::MAX_MACHINES;
    const DISK_JOBS: usize = 2 * Machine::MAX_MACHINES + 4;
    const COMPILE_JOBS: usize = (COMPILE_WINDOW + 1) * Machine::MAX_MACHINES;

    /// How long the program waits, once its windows are gone, for the store
    /// to take what it was last asked to keep.
    const DRAIN_NS: u64 = 5_000_000_000;

    /// How long a machine asked to suspend has to send its snapshot before it
    /// is turned off: many times what the largest takes.
    const SUSPEND_GRACE_NS: u64 = 10_000_000_000;

    /// Sound the front-end keeps beyond the stream's ring, and the latency it
    /// asks the stream for, in frames at its rate: a few frames' worth of
    /// the machine at sixty a second.
    const SOUND_WAITING_FRAMES: u32 = 16_384;
    const LATENCY_FRAMES: u32 = 2_048;

    /// What a stream is opened at when the service offers nothing better.
    const SOUND_RATE: u32 = 44_100;

    type Client = WindowClient<app::RtWindowTransport>;

    /// Every running machine's compiler.
    #[derive(Default)]
    struct Compilers {
        held: Vec<HeldCompiler>,
    }

    /// What the loop asks of the compile worker.
    enum CompileJob {
        /// Compile block request `request` of machine `machine`, its worker
        /// `worker`, from `payload`, the request its compiler is sent.
        Compile {
            machine: u64,
            worker: i64,
            request: u32,
            payload: Vec<u8>,
        },
        /// Machine `machine` is gone, and its compiler with it.
        Forget { machine: u64 },
    }

    /// What the compile worker answers.
    #[derive(Clone, Copy)]
    enum CompileDone {
        /// Machine `machine`'s block request `request`: where its code was
        /// installed, or none.
        Compiled {
            machine: u64,
            request: u32,
            code: Option<Installed>,
        },
        /// The same, with no code, and the machine to interpret from now on.
        Interpret { machine: u64, request: u32 },
        /// Machine `machine` is forgotten: nothing more is done for it.
        Forgot { machine: u64 },
    }

    type CompileWorker =
        Worker<Compilers, CompileJob, CompileDone, JobQueue<CompileJob, CompileDone>>;

    fn serve_compile(compilers: &mut Compilers, job: &mut CompileJob) -> CompileDone {
        match job {
            CompileJob::Compile {
                machine,
                worker,
                request,
                payload,
            } => {
                let (machine, request) = (*machine, *request);
                let at = match compilers
                    .held
                    .iter()
                    .position(|held| held.machine() == machine)
                {
                    Some(at) => Some(at),
                    None if compilers.held.try_reserve(1).is_ok() => {
                        compilers.held.push(HeldCompiler::new(machine, *worker));
                        Some(compilers.held.len() - 1)
                    }
                    None => None,
                };
                match at.map(|at| compilers.held[at].compile(payload)) {
                    Some(Ok(code)) => CompileDone::Compiled {
                        machine,
                        request,
                        code,
                    },
                    Some(Err(Interpret)) => CompileDone::Interpret { machine, request },
                    None => CompileDone::Compiled {
                        machine,
                        request,
                        code: None,
                    },
                }
            }
            CompileJob::Forget { machine } => {
                compilers.held.retain(|held| held.machine() != *machine);
                CompileDone::Forgot { machine: *machine }
            }
        }
    }

    /// How this front-end compiles its machines' blocks: the worker that does
    /// it, and the processor the code is for.
    struct Blocks {
        relay: Arc<CompileWorker>,
        target: tairix_codegen::Target,
    }

    /// The compile worker and the processor it compiles for, started on
    /// `set`; none, said why, where blocks cannot be compiled.
    fn start_blocks(set: u64) -> Option<Blocks> {
        let target = roles::compiled_for()
            .map_err(|why| app::report(APP_NAME, format_args!("machines are interpreted: {why}")))
            .ok()?;
        let Ok(relay) = Worker::queued(
            serve_compile,
            Compilers::default(),
            WorkerWake::create(),
            COMPILE_JOBS,
        ) else {
            app::report(
                APP_NAME,
                "machines are interpreted: there is no room for the compile worker",
            );
            return None;
        };
        let relay = Arc::new(relay);
        app::start_worker(APP_NAME, &relay, set, COMPILE_TOKEN, "compile");
        Some(Blocks { relay, target })
    }

    /// Hand block request `request` of machine `ticket`, its code to be
    /// installed in `worker`, to the compile worker as `payload`: answered
    /// with no code where there is none or the compile worker can take no
    /// more, and the machine interpreted where its blocks are not compiled.
    fn block_asked(
        blocks: Option<&Blocks>,
        (ticket, worker): (u64, Option<i64>),
        (request, payload): (u32, Option<Vec<u8>>),
        host: &mut Host<RtSessionChannel, LogSink>,
    ) {
        let (Some(blocks), Some(worker)) = (blocks, worker) else {
            let _ = host.answer_block(request, None);
            host.interpret();
            return;
        };
        let submitted = payload.is_some_and(|payload| {
            let job = CompileJob::Compile {
                machine: ticket,
                worker,
                request,
                payload,
            };
            blocks.relay.submit(job).is_ok()
        });
        if !submitted {
            let _ = host.answer_block(request, None);
        }
    }

    /// An image the front-end reads and writes through its descriptor.
    struct FileMedium(File);

    impl FileMedium {
        fn sync(&self) -> Result<(), String> {
            self.0
                .sync()
                .map_err(|err| format!("the disk could not be kept ({})", Errno::from_syscall(err)))
        }
    }

    impl Medium for FileMedium {
        fn read_at(&self, offset: u64, out: &mut [u8]) -> Result<usize, MediaError> {
            self.0.read_at(offset, out).map_err(|_| MediaError::Failed)
        }

        fn write_at(&self, offset: u64, data: &[u8]) -> Result<(), MediaError> {
            match self.0.write_at(offset, data) {
                Ok(n) if n == data.len() => Ok(()),
                _ => Err(MediaError::Failed),
            }
        }

        fn set_len(&self, len: u64) -> Result<(), MediaError> {
            self.0.truncate(len).map_err(|_| MediaError::Failed)
        }
    }

    /// A stream opened, with the transport its notifications are read
    /// through.
    struct Opened {
        stream: LiveStream,
        audio: RtAudio,
        rate: u32,
    }

    impl Opened {
        fn close(mut self) {
            let _ = self.stream.close(&mut self.audio);
        }
    }

    /// What starting a machine gave: its disks, its worker, its sound if the
    /// seat has any, and what it resumes.
    struct Launched {
        disks: [Option<FileMedium>; 2],
        session: RtSessionChannel,
        sound: Option<Opened>,
        resume: Resumable,
    }

    /// Why a machine's state is kept.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Keeping {
        /// It was turned off: its last screen.
        Off,
        /// It resumed: its last screen, its snapshot set aside before it runs.
        SetAside,
        /// It was suspended: its last screen, and its snapshot where its disks
        /// were kept first.
        Suspended,
    }

    /// What the loop asks of the library worker.
    enum Job {
        /// Read the library, sweep what no machine owns, and draw every
        /// machine's last screen at `picture`.
        Load { picture: Option<(u32, u32)> },
        /// Write a record of the library.
        Record(Record),
        /// Make machine `id`'s default disk for a controller recording
        /// `recording`.
        Make {
            id: u32,
            recording: Recording,
            serial: u32,
        },
        /// Copy machine `from`'s disks and last screen to machine `to`.
        Copy {
            from: u32,
            to: u32,
            drives: [bool; 2],
        },
        /// Remove every blob of machine `id`.
        Remove { id: u32 },
        /// Keep machine `id`'s last screen, and its snapshot while it is
        /// `suspended`, and draw the screen at `picture`; `why` it is kept.
        Keep {
            id: u32,
            screen: LastScreen,
            suspended: Option<Suspended>,
            picture: Option<(u32, u32)>,
            why: Keeping,
        },
        /// Draw these machines' last screens at `picture`.
        Pictures { ids: Vec<u32>, picture: (u32, u32) },
        /// Keep `screen` as a PNG in the file `grant` hands over.
        Shoot { screen: LastScreen, grant: u64 },
        /// Open machine `id`'s disks of `sectors` each and its sound, read
        /// its snapshot where it is `suspended`, and start its worker over a
        /// share of `share` bytes, for the start `ticket` names.
        Start {
            id: u32,
            ticket: u64,
            sectors: [Option<u64>; 2],
            share: usize,
            suspended: bool,
        },
    }

    /// What the library worker answers.
    enum Done {
        Loaded(Result<Shelved<Machine>, String>),
        Recorded(Result<u64, String>),
        Made {
            id: u32,
            disk: Result<StoredDisk, String>,
        },
        Copied {
            from: u32,
            to: u32,
            result: Result<(), String>,
        },
        Removed(Result<(), String>),
        Kept {
            id: u32,
            picture: Option<Vec<u8>>,
            why: Keeping,
            result: Result<(), String>,
        },
        Pictured(Vec<(u32, Vec<u8>)>),
        Shot(Result<(), String>),
        Started {
            id: u32,
            ticket: u64,
            launched: Result<Launched, String>,
        },
    }

    type StoreWorker = Worker<(), Job, Done, JobQueue<Job, Done>>;

    fn serve_store(_: &mut (), job: &mut Job) -> Done {
        match job {
            Job::Load { picture } => Done::Loaded(kept::load::<Machine>(*picture, &STATE_BOUNDS)),
            Job::Record(record) => Done::Recorded(kept::write_record(record)),
            Job::Make {
                id,
                recording,
                serial,
            } => Done::Made {
                id: *id,
                disk: make(*id, *recording, *serial),
            },
            Job::Copy { from, to, drives } => Done::Copied {
                from: *from,
                to: *to,
                result: copy(*from, *to, *drives),
            },
            Job::Remove { id } => Done::Removed(kept::remove::<Machine>(*id)),
            Job::Keep {
                id,
                screen,
                suspended,
                picture,
                why,
            } => Done::Kept {
                id: *id,
                picture: picture
                    .and_then(|size| thumbnail(screen.width, screen.height, &screen.pixels, size)),
                why: *why,
                result: kept::keep(*id, screen, suspended.as_ref(), &STATE_BOUNDS),
            },
            Job::Pictures { ids, picture } => {
                Done::Pictured(kept::pictures(ids, *picture, &STATE_BOUNDS))
            }
            Job::Shoot { screen, grant } => Done::Shot(shoot(screen, *grant)),
            Job::Start {
                id,
                ticket,
                sectors,
                share,
                suspended,
            } => Done::Started {
                id: *id,
                ticket: *ticket,
                launched: launch(*id, *sectors, *share, *suspended),
            },
        }
    }

    /// `screen` kept as a PNG, the size its monitor showed it at, in the file
    /// `grant` hands over: written from the start and cut to what was written.
    fn shoot(screen: &LastScreen, grant: u64) -> Result<(), String> {
        let file = File::from_delegation(grant)
            .map_err(|err| format!("the file could not be taken ({})", Errno::from_syscall(err)))?;
        let (width, height, rgba) =
            screenshot(screen.width, screen.height, &screen.pixels, VGA_SCREEN.0)
                .ok_or_else(|| String::from("there is not enough memory to draw it"))?;
        let picture = tairix_image::Picture::rgba(width, height, rgba)
            .map_err(|why| format!("it could not be made a picture ({why:?})"))?;
        let png = tairix_image::encode_png(&picture)
            .map_err(|why| format!("it could not be written as a PNG ({why:?})"))?;
        let failed = |err: i64| format!("it could not be written ({})", Errno::from_syscall(err));
        if file.write_at(0, &png).map_err(failed)? != png.len() {
            return Err(String::from("it could not be written whole"));
        }
        file.truncate(png.len() as u64).map_err(failed)?;
        file.sync().map_err(failed)
    }

    /// Machine `id`'s default disk, laid into its first drive's blob.
    fn make(id: u32, recording: Recording, serial: u32) -> Result<StoredDisk, String> {
        let made = default_disk(recording, serial)
            .map_err(|why| format!("its disk could not be made ({why:?})"))?;
        let medium = FileMedium(open_blob(&library::disk_blob(id, 0), BlobMode::ReadWrite)?);
        disk::lay(&medium, &made.device)
            .map_err(|err| format!("its disk could not be written ({err:?})"))?;
        medium.sync()?;
        Ok(StoredDisk {
            kind: made.kind,
            format: made.low_level,
        })
    }

    /// Machine `from`'s disks on `drives`, and its last screen if it has one,
    /// copied to machine `to`.
    fn copy(from: u32, to: u32, drives: [bool; 2]) -> Result<(), String> {
        for (drive, _) in drives.iter().enumerate().filter(|(_, held)| **held) {
            kept::copy_blob(
                &library::disk_blob(from, drive),
                &library::disk_blob(to, drive),
            )?;
        }
        kept::copy_state(from, to)
    }

    /// Machine `id`'s fixed disk `drive`, which must be `sectors` long.
    fn open_disk(id: u32, drive: usize, sectors: u64) -> Result<FileMedium, String> {
        let file = open_blob(&library::disk_blob(id, drive), BlobMode::ReadWrite)?;
        match file.regular_len().ok().flatten().unwrap_or(0) {
            0 => Err(String::from("its hard disk is missing")),
            held if Some(held) == sectors.checked_mul(SECTOR as u64) => Ok(FileMedium(file)),
            _ => Err(String::from("its hard disk is not the size of its drive")),
        }
    }

    /// Machine `id`'s disks opened, its snapshot read where it is
    /// `suspended`, its worker started, and its sound.
    fn launch(
        id: u32,
        sectors: [Option<u64>; 2],
        share: usize,
        suspended: bool,
    ) -> Result<Launched, String> {
        let resume = if suspended {
            kept::resumable(id, &STATE_BOUNDS)?
        } else {
            Resumable::Nothing
        };
        let mut disks = [None, None];
        for (drive, (held, sectors)) in disks.iter_mut().zip(sectors).enumerate() {
            if let Some(sectors) = sectors {
                *held = Some(open_disk(id, drive, sectors)?);
            }
        }
        let share =
            NonZeroUsize::new(share).ok_or_else(|| String::from("it has no screen to share"))?;
        let session = RtSessionChannel::launch(SPAWN_SELF, WorkerMemory::Documents, Some(share))
            .map_err(|err| format!("its worker could not be started ({err})"))?;
        Ok(Launched {
            disks,
            session,
            sound: open_sound(),
            resume,
        })
    }

    /// The sound stream, or none: a machine plays silently on a seat with no
    /// sound, and says so.
    fn open_sound() -> Option<Opened> {
        let mut audio = RtAudio::new();
        let params = OpenParams {
            device_id: 0,
            direction: StreamDirection::Playback,
            format: SampleFormat::S16,
            rate: Rate::new(SOUND_RATE).ok()?,
            channel_map: ChannelMap::STEREO,
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
        if grant.format != SampleFormat::S16 || grant.channel_map != ChannelMap::STEREO {
            app::report(
                APP_NAME,
                "the sound stream is not 16-bit stereo; the machine is silent",
            );
            let _ = stream.close(&mut audio);
            return None;
        }
        let rate = grant.rate.hz();
        if let Err(err) = stream.client_mut().start(&mut audio, Frames::new(0)) {
            app::report(
                APP_NAME,
                format_args!("the sound stream would not start ({err})"),
            );
            let _ = stream.close(&mut audio);
            return None;
        }
        Some(Opened {
            stream,
            audio,
            rate,
        })
    }

    /// A shared folder as its server reaches it: the directory the picker
    /// handed over, every name an entry of a directory held beneath it, so
    /// nothing a share does reaches outside the folder.
    mod host_folder {
        use super::alloc::string::String;
        use super::alloc::vec::Vec;
        use core::ops::ControlFlow;

        use tairix_abi::driver::filesystem::VolumeStats;
        use tairix_abi::fs::DirEntries;
        use tairix_abi::{
            Access as Want, AtFlags, Errno, FileStat, OpenFlags, RenameFlags, TimesChange,
            UnlinkFlags, READDIR_BATCH_MAX,
        };
        use tairix_pcemu::folder::host::{HostDir, HostEntry, HostFile};
        use tairix_pcemu::folder::server::ENTRIES;
        use tairix_pcemu::folder::{Access, Creation};
        use tairix_rt::{Dir, File};

        /// What a file and a directory the machine makes are born with, less
        /// the process's umask.
        const FILE_MODE: u32 = 0o666;
        const DIR_MODE: u32 = 0o777;

        /// A directory of the folder, held.
        pub struct Folder(pub Dir);

        /// A file of the folder, held.
        pub struct Opened(File);

        fn errno(raw: i64) -> Errno {
            Errno::from_syscall(raw)
        }

        impl HostDir for Folder {
            type File = Opened;

            fn list(&self) -> Result<Vec<HostEntry>, Errno> {
                let mut batch = Vec::new();
                batch
                    .try_reserve_exact(READDIR_BATCH_MAX)
                    .map_err(|_| Errno::OutOfMemory)?;
                batch.resize(READDIR_BATCH_MAX, 0);
                let mut entries: Vec<HostEntry> = Vec::new();
                let mut refused = None;
                self.0
                    .for_each_batch(&mut batch, |records| {
                        for record in DirEntries::new(records) {
                            let record = match record {
                                Ok(record) => record,
                                Err(why) => {
                                    refused = Some(why);
                                    return ControlFlow::Break(());
                                }
                            };
                            // A name the ABI does not let a program hold is
                            // none the machine can reach either.
                            let Ok(name) = core::str::from_utf8(record.name) else {
                                continue;
                            };
                            let mut held = String::new();
                            if entries.try_reserve(1).is_err()
                                || held.try_reserve_exact(name.len()).is_err()
                            {
                                refused = Some(Errno::OutOfMemory);
                                return ControlFlow::Break(());
                            }
                            held.push_str(name);
                            entries.push(HostEntry {
                                name: held,
                                id: record.id,
                            });
                            // No listing names more than a FAT directory holds.
                            if entries.len() == ENTRIES {
                                return ControlFlow::Break(());
                            }
                        }
                        ControlFlow::Continue(())
                    })
                    .map_err(errno)?;
                refused.map_or(Ok(entries), Err)
            }

            fn stat(&self) -> Result<FileStat, Errno> {
                self.0.as_file().stat().map_err(errno)
            }

            fn look(&self, name: &str) -> Result<FileStat, Errno> {
                let node = self.0.resolve(name.as_bytes(), true).map_err(errno)?;
                node.stat().map_err(errno)
            }

            fn may_write(&self, name: Option<&str>) -> bool {
                match name {
                    None => self.0.access(&[], Want::WRITE, AtFlags::EMPTY_PATH),
                    Some(name) => self
                        .0
                        .access(name.as_bytes(), Want::WRITE, AtFlags::empty()),
                }
                .is_ok()
            }

            fn enter(&self, name: &str) -> Result<Self, Errno> {
                self.0.enter(name.as_bytes()).map(Folder).map_err(errno)
            }

            fn open(
                &self,
                name: &str,
                access: Access,
                creation: Creation,
            ) -> Result<Opened, Errno> {
                let flags = match access {
                    Access::Read => OpenFlags::READ,
                    Access::Write => OpenFlags::WRITE,
                    Access::Both => OpenFlags::READ.union(OpenFlags::WRITE),
                };
                let name = name.as_bytes();
                match creation {
                    Creation::Existing => self.0.open_file(name, flags),
                    Creation::Truncate => {
                        self.0
                            .create_file(name, flags.union(OpenFlags::TRUNCATE), FILE_MODE)
                    }
                    Creation::New => {
                        self.0
                            .create_file(name, flags.union(OpenFlags::EXCLUSIVE), FILE_MODE)
                    }
                }
                .map(Opened)
                .map_err(errno)
            }

            fn make_dir(&self, name: &str) -> Result<(), Errno> {
                self.0.mkdir(name.as_bytes(), DIR_MODE).map_err(errno)
            }

            fn remove(&self, name: &str, directory: bool) -> Result<(), Errno> {
                let flags = if directory {
                    UnlinkFlags::DIRECTORY
                } else {
                    UnlinkFlags::empty()
                };
                self.0.unlink(name.as_bytes(), flags, None).map_err(errno)
            }

            fn rename(&self, from: &str, to_dir: &Self, to: &str) -> Result<(), Errno> {
                let (from, to) = (from.as_bytes(), to.as_bytes());
                self.0
                    .rename(from, &to_dir.0, to, RenameFlags::NO_REPLACE, None)
                    .map_err(errno)
            }

            fn space(&self) -> Result<VolumeStats, Errno> {
                self.0.as_file().volume_stats().map_err(errno)
            }
        }

        impl HostFile for Opened {
            fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, Errno> {
                self.0.read_at(offset, buf).map_err(errno)
            }

            fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<usize, Errno> {
                self.0.write_at(offset, bytes).map_err(errno)
            }

            fn set_len(&mut self, len: u64) -> Result<(), Errno> {
                self.0.truncate(len).map_err(errno)
            }

            fn stat(&self) -> Result<FileStat, Errno> {
                self.0.stat().map_err(errno)
            }

            fn set_times(&self, change: &TimesChange) -> Result<(), Errno> {
                self.0.set_times(change).map(drop).map_err(errno)
            }

            fn sync(&self) -> Result<(), Errno> {
                self.0.sync().map_err(errno)
            }
        }
    }

    /// One folder a running machine shares.
    struct HeldShare {
        id: u32,
        slot: u8,
        server: Server<host_folder::Folder>,
    }

    /// One image a running machine is served from.
    struct HeldMedium {
        id: u32,
        medium: u16,
        file: FileMedium,
    }

    /// Every running machine's open images and shared folders, and the
    /// epoch the next share's handles carry, so none of an earlier share's
    /// reaches a later one's files.
    #[derive(Default)]
    struct Disks {
        held: Vec<HeldMedium>,
        shares: Vec<HeldShare>,
        next_epoch: u32,
    }

    impl Disks {
        fn medium(&self, id: u32, medium: u16) -> Option<&FileMedium> {
            self.held
                .iter()
                .find(|held| held.id == id && held.medium == medium)
                .map(|held| &held.file)
        }
    }

    /// What the loop asks of the disk worker.
    enum DiskJob {
        /// Serve machine `id`'s image `medium` from `file`.
        Attach {
            id: u32,
            medium: u16,
            file: Option<FileMedium>,
        },
        /// Close machine `id`'s image `medium`, or every one it has.
        Forget { id: u32, medium: Option<u16> },
        /// Read `count` sectors of machine `id`'s image `medium` from `first`.
        Fetch {
            id: u32,
            medium: u16,
            first: u64,
            count: u16,
        },
        /// Write sector `sector` of it.
        Store {
            id: u32,
            medium: u16,
            sector: u64,
            data: Box<[u8; SECTOR]>,
        },
        /// Take the file `grant` hands over for machine `id`'s diskette drive
        /// `unit`, and say how it goes in.
        Take {
            id: u32,
            unit: u8,
            grant: u64,
            writable: bool,
        },
        /// Make everything machine `id` wrote to its images durable.
        Sync { id: u32 },
        /// Serve the directory `grant` hands over as machine `id`'s share
        /// `slot`, labelled `label` and writable where `writable`, on a clock
        /// `offset` seconds ahead of the host's.
        Share {
            id: u32,
            slot: u8,
            grant: u64,
            writable: bool,
            label: DosName,
            offset: i64,
        },
        /// Stop serving machine `id`'s share `slot`.
        Unshare { id: u32, slot: u8 },
        /// Answer machine `id`'s request `op` of its share `slot`, a write's
        /// bytes held in `bytes`.
        Folder {
            id: u32,
            slot: u8,
            op: folder::Op<'static>,
            bytes: Vec<u8>,
        },
        /// Machine `id`'s clock now runs `offset` seconds ahead of the host's.
        ClockOffset { id: u32, offset: i64 },
    }

    /// A share's answer, its bytes its own.
    enum Foldered {
        Reply(Reply<'static>),
        Read(Vec<u8>),
    }

    impl Foldered {
        fn reply(&self) -> Reply<'_> {
            match self {
                Self::Reply(reply) => *reply,
                Self::Read(bytes) => Reply::Read(bytes),
            }
        }
    }

    /// What the disk worker answers.
    enum DiskDone {
        Attached {
            id: u32,
            held: bool,
        },
        Forgot,
        Fetched {
            id: u32,
            answer: Result<Vec<u8>, MediaError>,
        },
        Stored {
            id: u32,
            answer: Result<(), MediaError>,
        },
        Taken {
            id: u32,
            unit: u8,
            taken: Result<(FileMedium, Form), String>,
        },
        Synced {
            id: u32,
            result: Result<(), String>,
        },
        Shared {
            id: u32,
            slot: u8,
            result: Result<(), String>,
        },
        Foldered {
            id: u32,
            answer: Result<Foldered, FolderRefusal>,
        },
    }

    type DiskWorker = Worker<Disks, DiskJob, DiskDone, JobQueue<DiskJob, DiskDone>>;

    fn serve_disk(disks: &mut Disks, job: &mut DiskJob) -> DiskDone {
        match job {
            DiskJob::Attach { id, medium, file } => {
                let held = match file.take() {
                    Some(file) if disks.held.try_reserve(1).is_ok() => {
                        disks.held.push(HeldMedium {
                            id: *id,
                            medium: *medium,
                            file,
                        });
                        true
                    }
                    _ => false,
                };
                DiskDone::Attached { id: *id, held }
            }
            DiskJob::Forget { id, medium } => {
                disks
                    .held
                    .retain(|held| held.id != *id || medium.is_some_and(|m| m != held.medium));
                if medium.is_none() {
                    disks.shares.retain(|held| held.id != *id);
                }
                DiskDone::Forgot
            }
            DiskJob::Fetch {
                id,
                medium,
                first,
                count,
            } => DiskDone::Fetched {
                id: *id,
                answer: disks
                    .medium(*id, *medium)
                    .ok_or(MediaError::Failed)
                    .and_then(|held| {
                        let mut out = Vec::new();
                        disk::fetch(held, *first, *count, &mut out).map(|()| out)
                    }),
            },
            DiskJob::Store {
                id,
                medium,
                sector,
                data,
            } => DiskDone::Stored {
                id: *id,
                answer: disks
                    .medium(*id, *medium)
                    .ok_or(MediaError::Failed)
                    .and_then(|held| disk::store(held, *sector, data)),
            },
            DiskJob::Take {
                id,
                unit,
                grant,
                writable,
            } => DiskDone::Taken {
                id: *id,
                unit: *unit,
                taken: take(*grant, *writable),
            },
            DiskJob::Sync { id } => DiskDone::Synced {
                id: *id,
                result: disks
                    .held
                    .iter()
                    .filter(|held| held.id == *id)
                    .try_for_each(|held| held.file.sync()),
            },
            DiskJob::Share { .. }
            | DiskJob::Unshare { .. }
            | DiskJob::Folder { .. }
            | DiskJob::ClockOffset { .. } => serve_sharing(disks, job),
        }
    }

    /// Carry out `job` of the machines' shared folders.
    fn serve_sharing(disks: &mut Disks, job: &DiskJob) -> DiskDone {
        match *job {
            DiskJob::Share {
                id,
                slot,
                grant,
                writable,
                label,
                offset,
            } => DiskDone::Shared {
                id,
                slot,
                result: share(disks, (id, slot), (grant, writable), label, offset),
            },
            DiskJob::Unshare { id, slot } => {
                disks
                    .shares
                    .retain(|held| held.id != id || held.slot != slot);
                DiskDone::Forgot
            }
            DiskJob::Folder {
                id,
                slot,
                ref op,
                ref bytes,
            } => DiskDone::Foldered {
                id,
                answer: serve_folder(disks, (id, slot), op, bytes),
            },
            DiskJob::ClockOffset { id, offset } => {
                for held in disks.shares.iter_mut().filter(|held| held.id == id) {
                    held.server.set_clock_offset(offset);
                }
                DiskDone::Forgot
            }
            _ => DiskDone::Forgot,
        }
    }

    /// Serve the directory `grant` hands over as machine `id`'s share `slot`.
    fn share(
        disks: &mut Disks,
        (id, slot): (u32, u8),
        (grant, writable): (u64, bool),
        label: DosName,
        offset: i64,
    ) -> Result<(), String> {
        let dir = tairix_rt::Dir::from_delegation(grant).map_err(|err| {
            format!(
                "the folder could not be taken ({})",
                Errno::from_syscall(err)
            )
        })?;
        disks
            .shares
            .try_reserve(1)
            .map_err(|_| String::from("there is no memory to serve it"))?;
        let epoch = disks.next_epoch;
        disks.next_epoch = epoch.wrapping_add(1) & 0x00FF_FFFF;
        let mut server = Server::new(host_folder::Folder(dir), label, writable, epoch);
        server.set_clock_offset(offset);
        disks.shares.push(HeldShare { id, slot, server });
        Ok(())
    }

    /// Answer machine `id`'s request `op` of its share `slot`, a write's
    /// bytes in `bytes`.
    fn serve_folder(
        disks: &mut Disks,
        (id, slot): (u32, u8),
        op: &folder::Op<'static>,
        bytes: &[u8],
    ) -> Result<Foldered, FolderRefusal> {
        let held = disks
            .shares
            .iter_mut()
            .find(|held| held.id == id && held.slot == slot)
            .ok_or(FolderRefusal::Gone)?;
        let op = match *op {
            folder::Op::Write { handle, offset, .. } => folder::Op::Write {
                handle,
                offset,
                bytes,
            },
            other => other,
        };
        match held.server.call(&op)?.detach() {
            Ok(reply) => Ok(Foldered::Reply(reply)),
            Err(read) => {
                let mut owned = Vec::new();
                owned
                    .try_reserve_exact(read.len())
                    .map_err(|_| FolderRefusal::Fault)?;
                owned.extend_from_slice(read);
                Ok(Foldered::Read(owned))
            }
        }
    }

    /// The file `grant` hands over, and how it goes into a diskette drive.
    fn take(grant: u64, writable: bool) -> Result<(FileMedium, Form), String> {
        let file = File::from_delegation(grant)
            .map_err(|err| format!("the file could not be taken ({})", Errno::from_syscall(err)))?;
        let len = file
            .regular_len()
            .ok()
            .flatten()
            .ok_or_else(|| String::from("it is not a file"))?;
        let mut head = [0u8; media::HEAD];
        let read = file.read_at(0, &mut head).unwrap_or(0);
        let form = media::form_of(len, &head[..read], writable).map_err(|unfit| {
            String::from(match unfit {
                Unfit::TooLarge => "it is larger than any diskette's archive",
                Unfit::NotADiskette => "it is not the image of a diskette",
            })
        })?;
        Ok((FileMedium(file), form))
    }

    /// The opened stream as the sound path appends to it.
    struct Ring<'a>(&'a mut LiveStream);

    impl Stream for Ring<'_> {
        fn append(&mut self, samples: &[u8]) -> Result<usize, StreamError> {
            let at = self
                .0
                .ring()
                .and_then(|ring| ring.producer_position())
                .map_err(|_| StreamError)?;
            let written = self.0.write_at(at, samples).map_err(|_| StreamError)?;
            Ok(usize::try_from(written.sample_frames)
                .unwrap_or(0)
                .saturating_mul(tairix_pcemu::link::AUDIO_FRAME))
        }

        fn queued(&mut self) -> u32 {
            self.0
                .ring()
                .and_then(|ring| ring.readable_frames())
                .unwrap_or(0)
        }
    }

    /// The bit a member of the program's own is noted woken under; none for a
    /// token that is not one.
    fn member_bit(token: u64) -> u64 {
        token
            .checked_sub(app::FIRST_APP_TOKEN)
            .and_then(|n| u32::try_from(n).ok())
            .and_then(|n| 1u64.checked_shl(n))
            .unwrap_or(0)
    }

    /// The tokens of the machine in `slot`: its read pipe, its write pipe and
    /// its sound.
    fn machine_tokens(slot: usize) -> [u64; 3] {
        let base = FIRST_MACHINE_TOKEN + TOKENS_PER_MACHINE * slot as u64;
        [base, base + 1, base + 2]
    }

    /// What woke the park, for the loop to serve.
    #[derive(Clone, Copy, Default)]
    struct Woke {
        members: u64,
        desktop: bool,
        pressure: bool,
        due: bool,
    }

    /// The park: the event mailbox, the memory-pressure band, the workers'
    /// answers, every machine's pipes and sound, and the next due step.
    struct RtEventSource<'a> {
        mailbox: EventMailbox,
        set: u64,
        wakes: [&'a WorkerWake; 2],
        /// The compile worker's, where there is one.
        compiles: Option<&'a WorkerWake>,
        woke: &'a Cell<Woke>,
        due: &'a Cell<Option<u64>>,
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
                None => {
                    woke.due = true;
                    Parked::Interrupted
                }
                Some(Wake::App(token)) => {
                    // A level peek: undrained, it reports ready for ever.
                    match token {
                        STORE_TOKEN => self.wakes[0].drain(),
                        DISK_TOKEN => self.wakes[1].drain(),
                        COMPILE_TOKEN => {
                            if let Some(wake) = self.compiles {
                                wake.drain();
                            }
                        }
                        _ => {}
                    }
                    woke.members |= member_bit(token);
                    Parked::Interrupted
                }
                Some(Wake::PressureChanged) => {
                    woke.pressure = true;
                    Parked::Interrupted
                }
                Some(Wake::DesktopChanged) => {
                    woke.desktop = true;
                    Parked::Interrupted
                }
                Some(Wake::Event | Wake::PressureUnchanged) => Parked::Served,
            };
            self.woke.set(woke);
            Ok(parked)
        }
    }

    /// What a dialog over the library window holds.
    enum Sheet {
        New(Box<Assistant>),
        Settings { id: u32, settings: Box<Settings> },
    }

    /// What a dialog answered.
    enum SheetAnswer {
        New(assistance::Answer),
        Settings(u32, settings::Answer),
    }

    impl library_window::Sheet for Sheet {
        type Answer = SheetAnswer;

        fn extent(&self, scale: Scale, theme: &Theme) -> (u32, u32) {
            match self {
                Self::New(assistant) => assistant.extent(scale, theme),
                Self::Settings { settings, .. } => settings.extent(scale, theme),
            }
        }

        fn render(&self, surface: &mut Surface, scale: Scale, theme: &Theme) {
            match self {
                Self::New(assistant) => assistant.render(surface, scale, theme),
                Self::Settings { settings, .. } => settings.render(surface, scale, theme),
            }
        }

        fn on_input(
            &mut self,
            event: &InputEvent,
            at_ns: u64,
            looks: (Scale, &Theme),
            damage: &mut Region,
        ) -> Option<SheetAnswer> {
            match self {
                Self::New(assistant) => assistant
                    .on_input(event, at_ns, looks, damage)
                    .map(SheetAnswer::New),
                Self::Settings { id, settings } => settings
                    .on_input(event, at_ns, looks, damage)
                    .map(|answer| SheetAnswer::Settings(*id, answer)),
            }
        }
    }

    /// A machine's window: its pane, what it shows and what it owes.
    struct Window {
        pane: WindowPane,
        surface: Surface,
        picture: Picture,
        owed: Owed,
        input: Input,
        hidden: bool,
        held_title: bool,
        /// The last draw failed and was said; the next is said only once one
        /// has drawn again.
        failing: bool,
        /// The drives and the machine's menu along the window's foot.
        bar: MachineBar,
    }

    impl Window {
        /// Where the picture sits in the window as it stands: above the bar.
        fn target(&self) -> Option<Target> {
            let above = u32::try_from(self.bar.bounds().top()).unwrap_or(0);
            fit(self.pane.mode().width_px, above)
        }

        /// What the pointer is over, once there is a picture.
        fn over(&self) -> Option<Over> {
            let target = self.target()?;
            let frame = self.picture.screen().size();
            (frame.0 > 0).then_some(Over {
                adapter: 0,
                frame,
                target,
            })
        }

        /// The whole window laid out, drawn again and owed: black about the
        /// picture, the picture, and the bar.
        fn redraw(&mut self, looks: (Scale, &Theme), drawing: Drawing<'_>) {
            let mode = *self.pane.mode();
            let height = MachineBar::height(looks.0, looks.1).min(mode.height_px);
            let top = i32::try_from(mode.height_px - height).unwrap_or(0);
            self.bar
                .place(Rect::new(0, top, mode.width_px, height), looks);
            self.surface.fill(Color::rgb(0, 0, 0));
            self.picture.invalidate();
            self.draw(drawing);
            self.bar.render(&mut self.surface, looks);
            self.owed.owe_whole();
        }

        /// Draw the picture through `tube` from now on.
        fn set_tube(&mut self, tube: Tube, drawing: Drawing<'_>) {
            self.picture.set_tube(tube);
            self.draw(drawing);
        }

        /// Draw the tube's next animation step, if it is due.
        fn step_tube(&mut self, drawing: Drawing<'_>) {
            if self.picture.step(drawing.now_ns) {
                self.draw(drawing);
            }
        }

        /// Paint `damage` of the bar, and owe it.
        fn paint_bar(&mut self, damage: &Region, looks: (Scale, &Theme)) {
            let bar = &self.bar;
            repaint(
                &self.pane,
                &mut self.surface,
                &mut self.owed,
                damage,
                |surface| {
                    bar.render(surface, looks);
                },
            );
        }

        /// Draw what the picture changed into the window, and owe it.
        fn draw(&mut self, drawing: Drawing<'_>) {
            let Some(target) = self.target() else {
                return;
            };
            match self.picture.draw(&mut self.surface, target, drawing) {
                Ok(drawn) => {
                    if !drawn.is_empty() {
                        let mode = *self.pane.mode();
                        self.owed.owe(&mode, Repaint::Reported, &drawn);
                    }
                    self.failing = false;
                }
                Err(why) => {
                    if !core::mem::replace(&mut self.failing, true) {
                        app::report(
                            APP_NAME,
                            format_args!("the screen could not be drawn ({why:?})"),
                        );
                    }
                }
            }
        }

        /// Do what the input asked of the desktop, the machine window titled
        /// for machine `name`.
        fn ask(&mut self, client: &mut Client, asks: Asks, name: &str) {
            let id = self.pane.id();
            if let Some(lock) = asks.lock {
                if let Err(err) = client.lock_pointer(id, lock) {
                    app::report(
                        APP_NAME,
                        format_args!("the pointer lock was refused ({err})"),
                    );
                }
            }
            if let Some(hidden) = asks.hidden {
                if hidden != self.hidden {
                    self.hidden = hidden;
                    let shape = if hidden {
                        CursorShape::None
                    } else {
                        CursorShape::Arrow
                    };
                    let _ = client.set_cursor(id, shape);
                }
            }
            let held = self.input.locked();
            if held != self.held_title {
                self.held_title = held;
                let title = if held {
                    held_title(name)
                } else {
                    String::from(name)
                };
                let _ = client.set_title(id, &title);
            }
        }
    }

    /// Route a pointer event at `(x, y)` doing `action` over machine window
    /// `window`: a press on the bar, and the release of one, are the bar's;
    /// the rest the guest's input, its motion the bar's too. What the input
    /// asks of the desktop, and the menu the bar asks for.
    fn pointer(
        window: &mut Window,
        host: Option<&mut Host<RtSessionChannel, LogSink>>,
        (x, y, action): (u32, u32, PointerAction),
        looks: (Scale, &Theme),
    ) -> (Asks, Option<MenuRequest>) {
        let point = pointer_point(x, y);
        let bars = match action {
            PointerAction::Pressed(_) => {
                window.bar.bounds().contains(point) && !window.input.holding()
            }
            PointerAction::Released(_) => window.bar.is_pressing(),
            PointerAction::Moved | PointerAction::Cancelled => false,
        };
        let mut request = None;
        let mut damage = Region::new();
        let bar = &mut window.bar;
        pointer_input_events(action, point)
            .filter(|input| {
                bars || matches!(
                    input,
                    InputEvent::PointerMoved { .. } | InputEvent::PointerCancelled
                )
            })
            .for_each(|input| {
                let asked = bar.on_input(&input, &mut damage);
                request = request.or(asked);
            });
        if !damage.is_empty() {
            window.paint_bar(&damage, looks);
        }
        let mut asks = Asks::default();
        if !bars {
            if let (Some(host), Some(over)) = (host, window.over()) {
                asks = window.input.pointer(host, over, (point.x, point.y), action);
            }
        }
        (asks, request)
    }

    /// Open the menu `request` names, over window `window_id`, for a machine
    /// `shown` as it stands: its open, once the desktop has it.
    fn open_bar_menu(
        client: &mut Client,
        window_id: u64,
        request: MenuRequest,
        shown: &Shown,
    ) -> Option<Followed<Menu>> {
        let menu = match request.menu {
            Menu::Drive { holds, .. } => drive_menu(holds),
            Menu::Share { .. } => share_menu(),
            Menu::Machine => machine_menu::menu(shown).ok(),
        }?;
        let anchor = request.anchor;
        let region =
            WindowRegion::new(anchor.left(), anchor.top(), anchor.width, anchor.height).ok()?;
        match client.open_menu(window_id, region, &menu) {
            Ok(open) => Some(Followed::new(open, request.menu, menu)),
            Err(err) => {
                app::report(
                    APP_NAME,
                    format_args!("the menu could not be opened ({err})"),
                );
                None
            }
        }
    }

    /// A diskette drive's menu, its Eject offered where it `holds` an image.
    fn drive_menu(holds: bool) -> Option<AppMenu> {
        let item = |id, label, symbol| {
            Some(AppMenuItem::new(
                AppMenuItemId::new(id).ok()?,
                AppMenuLabel::new(label).ok()?,
                symbol,
            ))
        };
        let insert = item(INSERT_ROW, "Insert Disk\u{2026}", AppMenuSymbol::Open)?;
        let eject = item(EJECT_ROW, "Eject", AppMenuSymbol::Drive)?;
        let mut menu = AppMenu::EMPTY;
        menu.push(AppMenuRow::Item(insert)).ok()?;
        let eject = if holds {
            eject.repeatable()
        } else {
            eject.disabled()
        };
        menu.push(AppMenuRow::Item(eject)).ok()?;
        Some(menu)
    }

    /// A shared folder's menu.
    fn share_menu() -> Option<AppMenu> {
        let stop = AppMenuItem::new(
            AppMenuItemId::new(STOP_SHARING_ROW).ok()?,
            AppMenuLabel::new("Stop Sharing").ok()?,
            AppMenuSymbol::Close,
        );
        let mut menu = AppMenu::EMPTY;
        menu.push(AppMenuRow::Item(stop)).ok()?;
        Some(menu)
    }

    /// What a machine window's picture is drawn with now.
    fn drawing<'a>(pool: &'a LazyPool, desktop: &Desktop) -> Drawing<'a> {
        Drawing {
            percent: desktop.scale().percent(),
            runner: pool.runner(),
            refresh: desktop.refresh(),
            now_ns: tairix_rt::clock_get(),
        }
    }

    /// Open a machine window titled `title`.
    fn open_machine_window(
        client: &mut Client,
        (event_endpoint, title, bar, tube): (u64, &str, MachineBar, Tube),
        (desktop, theme, drawing): (&Desktop, &Theme, Drawing<'_>),
    ) -> Result<Window, String> {
        let (width, height) = opening_extent(MachineBar::height(desktop.scale(), theme));
        let (width, height) = (
            width.min(desktop.screen_width_px()),
            height.min(desktop.screen_height_px()),
        );
        let least = desktop.window_size(LEAST_SIZE.0, LEAST_SIZE.1);
        let mode = app::mode_for(width, height);
        let surface = Surface::new(mode.width_px, mode.height_px)
            .ok_or_else(|| String::from("there is not enough memory to draw its window"))?;
        let sizing = WindowSizing::Resizable {
            min_width_px: least.0,
            min_height_px: least.1,
            max_width_px: 0,
            max_height_px: 0,
        };
        let pane = WindowPane::open(client, event_endpoint, &mode, title, sizing)
            .map_err(|err| format!("the desktop would not open its window ({err})"))?;
        if let Err(err) = client.set_keyboard_mode(pane.id(), KeyboardMode::Raw) {
            app::report(
                APP_NAME,
                format_args!("the keyboard cannot be read raw ({err})"),
            );
        }
        let mut window = Window {
            pane,
            surface,
            picture: Picture::new(tube),
            owed: Owed::new(),
            input: Input::new(),
            hidden: false,
            held_title: false,
            failing: false,
            bar,
        };
        window.redraw((desktop.scale(), theme), drawing);
        Ok(window)
    }

    /// A machine as the front-end runs it: its session and the members of
    /// the wait-set its pipes are registered as.
    struct Running {
        host: Host<RtSessionChannel, LogSink>,
        members: SessionMembers,
        /// Its worker's process, which the blocks it asks for are installed
        /// in.
        worker: Option<i64>,
    }

    /// A diskette image a machine window knows of.
    #[derive(Clone, Debug)]
    struct Diskette {
        name: String,
        read_only: bool,
        medium: u16,
    }

    /// What a machine window's pick is for.
    enum Pick {
        /// An image for diskette drive `unit`.
        Diskette(u8),
        /// Where this screen, as it stood when it was asked for, is kept.
        Screenshot(LastScreen),
        /// A folder to share.
        Share,
    }

    /// A folder a machine window shares, or is about to.
    struct MachineShare {
        /// The folder's name, and the label DOS reads it under.
        name: String,
        label: DosName,
        writable: bool,
        /// The drive DOS gave it, `0` for `A:`, once it has one.
        drive: Option<u8>,
    }

    impl MachineShare {
        fn shown(&self) -> ShareShown {
            ShareShown {
                name: self.name.clone(),
                drive: self.drive,
                read_only: !self.writable,
            }
        }
    }

    /// A folder picked to share, waiting for its name: the grant handing it
    /// over, and whether it may be changed.
    #[derive(Clone, Copy)]
    struct Sharing {
        grant: u64,
        writable: bool,
    }

    /// Where a machine with a window is in its life.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Life {
        /// Its disks, worker and sound are being opened.
        Starting,
        /// Its worker builds the machine afresh, or takes back its snapshot.
        Opening(Opening),
        /// Resumed: its snapshot is being set aside before it runs, so a
        /// machine cut short is never resumed again from under its disks.
        SettingAside,
        /// Running, or paused.
        Running,
        /// Asked to close: its window closes once its snapshot has come.
        Suspending,
    }

    /// What a machine's worker opens with.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Opening {
        /// A machine built afresh.
        Fresh,
        /// A machine's snapshot taken back.
        Resuming,
    }

    /// A machine with a window: started, or starting.
    struct MachineWindow {
        id: u32,
        life: Life,
        /// Whether the worker took the snapshot back, once it has said.
        resumed: Option<bool>,
        /// It has been told to run, so what it shows is worth keeping.
        ran: bool,
        /// When, asked to suspend, it must have sent its snapshot.
        answer_by: Option<u64>,
        /// Which start this window is, so an answer to an earlier one, closed
        /// while it started, is not taken for it.
        ticket: u64,
        name: String,
        /// Which of the machines' token ranges is its own.
        slot: usize,
        window: Window,
        run: Option<Running>,
        sound: Option<(Opened, Sound)>,
        /// The menu up, what it is for, and the rows it was last given.
        menu: Option<Followed<Menu>>,
        /// What a pick is for, and then the diskette drive its name is, or
        /// the folder to share it names.
        picking: Option<Pick>,
        naming: Option<u8>,
        sharing: Option<Sharing>,
        /// The folders it shares, by slot: each once the disk worker serves
        /// it.
        shares: [Option<MachineShare>; SHARES],
        /// Whether the machine is paused, the rate its processor runs at, and
        /// whether its window fills the display.
        paused: bool,
        speed: CpuClock,
        fullscreen: bool,
        /// Whether its screen shows text, as the worker last said, and
        /// whether the clipboard was asked for to be typed on its keyboard.
        clipboard: Clipboard,
        /// What each diskette drive holds, and what is going into it.
        held: [Option<Diskette>; 4],
        coming: [Option<Diskette>; 4],
        /// Its debugger's window, while one is open.
        debugger: Option<Box<DebugWindow>>,
    }

    impl MachineWindow {
        /// The machine as its menus show it now.
        fn shown(&self) -> Shown {
            Shown {
                paused: self.paused,
                speed: self.speed,
                tube: self.window.picture.tube(),
                clipboard: Clipboard {
                    pasting: self.clipboard.pasting
                        || self.run.as_ref().is_some_and(|run| run.host.typing()),
                    ..self.clipboard
                },
                can_share: self.shares.iter().any(Option::is_none),
                windows: Windows {
                    fullscreen: self.fullscreen,
                    debugging: self.debugger.is_some(),
                },
            }
        }

        /// The rows of `menu` as the machine stands now.
        fn menu_of(&self, menu: Menu) -> Option<AppMenu> {
            match menu {
                Menu::Machine => machine_menu::menu(&self.shown()).ok(),
                Menu::Drive { unit, .. } => drive_menu(self.held.get(usize::from(unit))?.is_some()),
                Menu::Share { .. } => share_menu(),
            }
        }
    }

    /// A machine's debugger window, and the dialog it adds a breakpoint
    /// through.
    struct DebugWindow {
        pane: WindowPane,
        surface: Surface,
        owed: Owed,
        debugger: Debugger,
        adding: Option<AddingWindow>,
    }

    /// The dialog a breakpoint is added through, over its debugger.
    struct AddingWindow {
        pane: WindowPane,
        surface: Surface,
        owed: Owed,
        adding: Adding,
    }

    /// Paint `damage` of a debugger window, and owe it.
    fn paint_debugger(win: &mut DebugWindow, damage: &Region, looks: (Scale, &Theme)) {
        if damage.is_empty() {
            return;
        }
        win.debugger.render(&mut win.surface, damage, looks);
        let mode = *win.pane.mode();
        win.owed.owe(&mode, Repaint::Reported, damage);
    }

    /// The debugger's report `heard`, shown in `win`: what the window asks
    /// next.
    fn debugger_heard(
        win: &mut DebugWindow,
        heard: HostEvent<'_>,
        looks: (Scale, &Theme),
    ) -> Option<Ask> {
        let mut damage = Region::new();
        let ask = match heard {
            HostEvent::Stopped { reason, registers } => {
                win.debugger.stopped(reason, registers, looks, &mut damage)
            }
            HostEvent::Seen(seen) => win.debugger.seen(&seen, looks, &mut damage),
            _ => None,
        };
        paint_debugger(win, &damage, looks);
        ask
    }

    /// Hand debugger `debugger`'s `ask` to its machine's `host`: whether it
    /// asks for the dialog that adds a breakpoint, which the host has no part
    /// in.
    fn relay(
        host: &mut Host<RtSessionChannel, LogSink>,
        debugger: &mut Debugger,
        ask: Ask,
    ) -> bool {
        let sent = match ask {
            Ask::Break => host.debug_break(),
            Ask::Go(go) => host.debug_go(go),
            Ask::Add(breakpoint) => host.add_breakpoint(breakpoint),
            Ask::Remove(breakpoint) => host.remove_breakpoint(breakpoint),
            Ask::Look(look) => host
                .look(look)
                .map(|_| ())
                .inspect_err(|_| debugger.unanswered()),
            Ask::AddBreakpoint => return true,
        };
        if let Err(why) = sent {
            app::report(
                APP_NAME,
                format_args!("the debugger's request was refused ({why:?})"),
            );
        }
        false
    }

    /// Share `slot` of `shares` is drive `drive`, or took none: `bar` says so.
    fn shared(
        (shares, bar): (&mut [Option<MachineShare>; SHARES], &mut MachineBar),
        (slot, drive): (u8, Option<u8>),
        looks: (Scale, &Theme),
        damage: &mut Region,
    ) {
        if let Some(Some(share)) = shares.get_mut(usize::from(slot)) {
            share.drive = drive;
            let shown = share.shown();
            bar.set_share(slot, Some(shown), looks, damage);
        }
    }

    impl MachineWindow {
        /// Ask `client` for a folder to share: why not, where it was not.
        fn pick_share(&mut self, client: &mut Client) -> Option<String> {
            if self.shares.iter().all(Option::is_some) {
                return Some(String::from(
                    "as many folders as a machine takes are shared",
                ));
            }
            match client.pick_file(self.window.pane.id(), PickPurpose::Subtree) {
                Ok(()) => {
                    self.picking = Some(Pick::Share);
                    None
                }
                Err(err) => Some(format!("the picker could not be asked for ({err})")),
            }
        }
    }

    /// A machine taken out of the running, with the window it ran in, still
    /// open for its caller to close now or once its state is kept.
    struct Retired {
        id: u32,
        name: String,
        /// Whether it ran.
        ran: bool,
        /// The screen it last showed.
        screen: Option<LastScreen>,
        pane: WindowPane,
    }

    /// Where machine `id` stands: running under a window, being kept as it
    /// suspends, suspended, or off.
    fn standing_of(
        (machines, keeping, suspended): (&[MachineWindow], &Suspends<WindowPane>, &[u32]),
        id: u32,
    ) -> Standing {
        if machines.iter().any(|machine| machine.id == id) {
            Standing::Running
        } else if keeping.holds(id) {
            Standing::Saving
        } else if suspended.contains(&id) {
            Standing::Suspended
        } else {
            Standing::Off
        }
    }

    /// The setup machine `machine` starts with at a stream of `rate`, its
    /// disks `sectors` long; none without secure bytes to key its cache
    /// with, so no guest can choose which of its sectors collide.
    fn setup(machine: &Machine, sectors: [Option<u64>; 2], rate: u32) -> Option<Setup> {
        let mut config = machine.config;
        config.sound.stream_rate = rate;
        let mut seed = [0u8; tairix_hash::HashSeed::LEN];
        tairix_rt::random_fill(&mut seed).ok()?;
        let clock = kept::calendar_secs().map(|secs| machine.clock_at(secs));
        let mut fixed = [None, None];
        for (drive, slot) in fixed.iter_mut().enumerate() {
            let (Some(disk), Some(sectors)) = (&machine.disks[drive], sectors[drive]) else {
                continue;
            };
            *slot = Some(FixedDisk {
                medium: medium_of(drive),
                sectors,
                read_only: false,
                kind: disk.kind,
                format: disk.format.clone(),
            });
        }
        Some(Setup {
            config,
            fixed,
            audio_target: LATENCY_FRAMES,
            room: worker_ceiling(WorkerMemory::Documents),
            seed,
            band: tairix_rt::pressure::gauge().band(),
            clock,
            resume: None,
        })
    }

    /// The session's name for fixed disk `drive`.
    fn medium_of(drive: usize) -> u16 {
        if drive == 0 {
            1
        } else {
            2
        }
    }

    /// What of a machine's event needs nothing it lends: an event of its life,
    /// a request of its images or its shares — one whose bytes there was no
    /// memory to hold refused as a fault of the share's — or a block to
    /// compile, as its compiler is asked for it where blocks are compiled for
    /// `target`.
    enum Owned {
        Moment(Moment),
        Handed(Handed),
    }

    /// A machine's request another worker answers.
    enum Handed {
        Disk(DiskJob),
        Unserved,
        Block {
            request: u32,
            payload: Option<Vec<u8>>,
        },
    }

    /// Hand machine `asked`'s request `handed` to the worker that answers
    /// it, or answer it failed.
    fn hand_on(
        handed: Handed,
        (disks, blocks): (&DiskWorker, Option<&Blocks>),
        asked: (u64, Option<i64>),
        host: &mut Host<RtSessionChannel, LogSink>,
    ) {
        match handed {
            Handed::Disk(job) => ask_disk(disks, job, host),
            Handed::Unserved => {
                let _ = host.answer_folder(Err(FolderRefusal::Fault));
            }
            Handed::Block { request, payload } => {
                block_asked(blocks, asked, (request, payload), host);
            }
        }
    }

    /// `event` of machine `id`, where it needs nothing it lends.
    fn owned(
        id: u32,
        event: &HostEvent<'_>,
        target: Option<tairix_codegen::Target>,
    ) -> Option<Owned> {
        Some(match *event {
            HostEvent::Compile {
                request,
                model,
                cs,
                ip,
                bytes,
            } => Owned::Handed(Handed::Block {
                request,
                payload: target.and_then(|target| {
                    let key = Key { cs, ip };
                    Compile {
                        target,
                        model,
                        key,
                        bytes,
                    }
                    .encode()
                }),
            }),
            HostEvent::Ready => Owned::Moment(Moment::Ready),
            HostEvent::Resumed(resumed) => Owned::Moment(Moment::Resumed(resumed)),
            HostEvent::Suspended(outcome) => Owned::Moment(Moment::Suspended(outcome)),
            HostEvent::Fetch {
                medium,
                first,
                count,
            } => Owned::Handed(Handed::Disk(DiskJob::Fetch {
                id,
                medium,
                first,
                count,
            })),
            HostEvent::Store {
                medium,
                sector,
                data,
            } => Owned::Handed(Handed::Disk(DiskJob::Store {
                id,
                medium,
                sector,
                data: Box::new(*data),
            })),
            HostEvent::Folder { slot, op } => {
                let (op, written) = op.detach();
                let mut bytes = Vec::new();
                if let Some(written) = written {
                    if bytes.try_reserve_exact(written.len()).is_err() {
                        return Some(Owned::Handed(Handed::Unserved));
                    }
                    bytes.extend_from_slice(written);
                }
                Owned::Handed(Handed::Disk(DiskJob::Folder {
                    id,
                    slot,
                    op,
                    bytes,
                }))
            }
            _ => return None,
        })
    }

    /// Hand a machine's request of its images or its shares to the disk
    /// worker, answering it failed where the worker can take no more.
    fn ask_disk(disks: &DiskWorker, job: DiskJob, host: &mut Host<RtSessionChannel, LogSink>) {
        let Err(job) = disks.submit(job) else {
            return;
        };
        let _ = match job {
            DiskJob::Fetch { .. } => host.answer_fetch(Err(MediaError::Failed)),
            DiskJob::Folder { .. } => host.answer_folder(Err(FolderRefusal::Fault)),
            _ => host.answer_store(Err(MediaError::Failed)),
        };
    }

    /// An event of a machine's life, which needs nothing but its session.
    #[derive(Clone, Copy)]
    enum Moment {
        Ready,
        Resumed(bool),
        Suspended(Result<(), Unkept>),
    }

    /// What of a machine an event of its life changes.
    struct Lived<'a> {
        life: &'a mut Life,
        ran: &'a mut bool,
        resumed: &'a mut Option<bool>,
        screen: &'a Screen,
    }

    impl Lived<'_> {
        /// Take `moment`, the machine's session `host`: what is left to do,
        /// if anything.
        fn take(self, moment: Moment, host: &mut Host<RtSessionChannel, LogSink>) -> Option<Leave> {
            match moment {
                Moment::Ready => match *self.life {
                    Life::Opening(Opening::Resuming) => {
                        *self.life = Life::SettingAside;
                        Some(match LastScreen::of(self.screen) {
                            Some(screen) => Leave::SetAside(screen),
                            None => {
                                Leave::Ended(String::from("there is no memory to keep its screen"))
                            }
                        })
                    }
                    Life::Opening(Opening::Fresh) => {
                        *self.ran = host.run().is_ok();
                        *self.life = Life::Running;
                        None
                    }
                    // A machine closing is kept, not run.
                    Life::Starting | Life::SettingAside | Life::Running | Life::Suspending => None,
                },
                Moment::Resumed(resumed) => {
                    *self.resumed = Some(resumed);
                    None
                }
                Moment::Suspended(Ok(())) => Some(match host.take_kept() {
                    Some(kept) => Leave::Suspended(kept),
                    None => Leave::Ended(String::from("its snapshot was lost")),
                }),
                Moment::Suspended(Err(why)) => Some(Leave::Ended(format!(
                    "its state could not be kept ({}), so it was turned off",
                    unkept_because(why)
                ))),
            }
        }
    }

    /// What is left to do of a machine's leaving or resuming.
    enum Leave {
        /// It ended, for this reason.
        Ended(String),
        /// It suspended, and this is its snapshot.
        Suspended(Vec<u8>),
        /// It resumed and waits to run: its snapshot is to be set aside, its
        /// screen kept as it stands.
        SetAside(LastScreen),
    }

    /// Why a machine's snapshot was not kept, for the one who asked.
    const fn unkept_because(why: Unkept) -> &'static str {
        match why {
            Unkept::Memory => "its worker had no memory to take it",
            Unkept::Size => "it is larger than any snapshot can be",
        }
    }

    /// What a machine's event left the application to do.
    #[derive(Default)]
    struct Settled {
        /// The library changed.
        changed: bool,
        /// Why the machine is gone, if it is.
        ended: Option<String>,
    }

    impl Settled {
        fn fold(&mut self, more: Self) {
            self.changed |= more.changed;
            self.ended = self.ended.take().or(more.ended);
        }
    }

    /// What a machine's event touches beside its session.
    struct Parts<'a> {
        id: u32,
        diskettes: (&'a mut [Option<Diskette>; 4], &'a mut [Option<Diskette>; 4]),
        bar: &'a mut MachineBar,
        disks: &'a DiskWorker,
        library: &'a mut Library<Machine>,
        notes: &'a mut Vec<(String, String)>,
        damage: &'a mut Region,
    }

    /// Settle a machine's event that needs nothing more of its session: what
    /// the library keeps of it, its diskettes, its lamps, and its end.
    fn remember_event(event: HostEvent<'_>, parts: Parts<'_>) -> Settled {
        let Parts {
            id,
            diskettes,
            bar,
            disks,
            library,
            notes,
            damage,
        } = parts;
        let mut settled = Settled::default();
        match event {
            HostEvent::ClockSet(time) => {
                settled.changed = remember(library, id, |held| {
                    held.clock_offset = library::clock_offset(&time, kept::calendar_secs()?)?;
                    Some(())
                });
                if let Some(held) = library.machine(id).filter(|_| settled.changed) {
                    let offset = held.clock_offset;
                    if disks.submit(DiskJob::ClockOffset { id, offset }).is_err() {
                        app::report(
                            APP_NAME,
                            "the shared folders keep the clock the machine had",
                        );
                    }
                }
            }
            HostEvent::Formatted { drive, format } => {
                settled.changed = remember(library, id, |held| {
                    held.disks.get_mut(usize::from(drive))?.as_mut()?.format = format;
                    Some(())
                });
            }
            HostEvent::Inserted {
                drive,
                medium,
                outcome,
            } => {
                if let Err(refusal) = inserted(diskettes, bar, (drive, medium, outcome), damage) {
                    let _ = disks.submit(DiskJob::Forget {
                        id,
                        medium: Some(medium),
                    });
                    notes.push((
                        String::from("The disk could not be inserted"),
                        String::from(refused_because(refusal)),
                    ));
                }
            }
            HostEvent::Ejected { drive, medium } => {
                let _ = disks.submit(DiskJob::Forget {
                    id,
                    medium: Some(medium),
                });
                ejected(diskettes.1, bar, (drive, medium), damage);
            }
            HostEvent::Lights(lights) => bar.set_lights(lights, damage),
            HostEvent::Refused(refusal) => {
                settled.ended = Some(format!(
                    "the machine refused its configuration ({refusal:?})"
                ));
            }
            HostEvent::Ended(ending) => settled.ended = Some(ended_because(ending)),
            HostEvent::Ready
            | HostEvent::Frame { .. }
            | HostEvent::OutOfRange { .. }
            | HostEvent::Audio(_)
            | HostEvent::Fetch { .. }
            | HostEvent::Store { .. }
            | HostEvent::Pointing { .. }
            | HostEvent::Resumed(_)
            | HostEvent::Suspended(_)
            | HostEvent::Text(_)
            | HostEvent::TextShown(_)
            | HostEvent::Folder { .. }
            | HostEvent::Shared { .. }
            | HostEvent::Compile { .. }
            | HostEvent::Interpreting
            | HostEvent::Stopped { .. }
            | HostEvent::Seen(_) => {}
        }
        settled
    }

    /// Put the text a machine's screen shows on the clipboard, from its
    /// window `window_id`; why nothing was, where nothing was.
    fn copy_text(
        client: &mut Client,
        window_id: u64,
        outcome: Result<Text<'_>, Unread>,
    ) -> Result<(), String> {
        let shown = outcome.map_err(|why| {
            String::from(match why {
                Unread::NoText => "the screen shows graphics",
                Unread::TooLarge => "the screen holds more text than a copy takes",
                Unread::Memory => "there was no memory to read the screen into",
            })
        })?;
        let copied = text::copied(shown.columns, shown.codes)
            .map_err(|_| String::from("there was no memory for its text"))?;
        if copied.is_empty() {
            return Err(String::from("the screen is blank"));
        }
        client
            .set_clipboard(window_id, ClipboardKind::Text, copied.as_bytes())
            .map_err(|err| format!("the clipboard would not take it ({err})"))
    }

    /// Diskette drive `drive` took image `medium`, or refused it: the image
    /// that was coming is what the drive holds, and the bar says so.
    ///
    /// # Errors
    ///
    /// The machine's refusal, for its image to be forgotten.
    fn inserted(
        (coming, held): (&mut [Option<Diskette>; 4], &mut [Option<Diskette>; 4]),
        bar: &mut MachineBar,
        (drive, medium, outcome): (u8, u16, Result<(), InsertRefusal>),
        damage: &mut Region,
    ) -> Result<(), InsertRefusal> {
        outcome?;
        let unit = usize::from(drive);
        let arrived = coming
            .get_mut(unit)
            .and_then(Option::take)
            .filter(|coming| coming.medium == medium);
        if let (Some(held), Some(arrived)) = (held.get_mut(unit), arrived) {
            let shown = (shown_name(&arrived.name), arrived.read_only);
            *held = Some(arrived);
            bar.set_held(drive, Some(shown), damage);
        }
        Ok(())
    }

    /// Image `medium` came out of diskette drive `drive`.
    fn ejected(
        held: &mut [Option<Diskette>; 4],
        bar: &mut MachineBar,
        (drive, medium): (u8, u16),
        damage: &mut Region,
    ) {
        let slot = held.get_mut(usize::from(drive));
        if let Some(slot) =
            slot.filter(|slot| slot.as_ref().is_some_and(|disk| disk.medium == medium))
        {
            *slot = None;
            bar.set_held(drive, None, damage);
        }
    }

    /// What a diskette image is shown as: its name, or what it is where the
    /// name is not known.
    fn shown_name(name: &str) -> String {
        if name.is_empty() {
            String::from("disk image")
        } else {
            String::from(name)
        }
    }

    /// Why the machine would not take a diskette, as the user is told.
    const fn refused_because(refusal: InsertRefusal) -> &'static str {
        match refusal {
            InsertRefusal::NoDrive => "The machine has no such drive.",
            InsertRefusal::Medium => "The machine refused the image it was handed.",
            InsertRefusal::Format => "It is not the image of a diskette this drive takes.",
            InsertRefusal::Archive => "The ImageDisk archive could not be read.",
        }
    }

    /// The alert's headline when a machine could not start.
    const NOT_STARTED: &str = "The machine could not start";

    /// Why the machine's worker is gone, as the user is told.
    fn ended_because(ending: Ending) -> String {
        match ending {
            Ending::Finished => String::from("the machine ended"),
            Ending::Failed => String::from("the machine's worker failed"),
            Ending::Violation(why) => format!("the machine's worker was ended: {why}"),
        }
    }

    /// How far the library has been read.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Shelving {
        Reading,
        Read,
        Unread,
    }

    /// Everything the loop holds.
    struct App {
        client: Client,
        event_endpoint: u64,
        set: u64,
        desktop: Desktop,
        themes: ThemeRegistry,
        store: Arc<StoreWorker>,
        disks: Arc<DiskWorker>,
        /// How the machines' blocks are compiled, where they are.
        blocks: Option<Blocks>,
        /// Sessions of machines turned off whose compiler may still be
        /// installing code, by machine: each worker is ended but not reaped,
        /// so no other process can take its number, until the compile worker
        /// has forgotten the machine.
        retiring: Vec<(u64, Host<RtSessionChannel, LogSink>)>,
        library: Library<Machine>,
        /// Whether the library has been read: until it is, nothing is made
        /// or written that could land on what it holds.
        shelving: Shelving,
        /// Machines minted whose disks are being made or copied.
        pending: Vec<Machine>,
        /// The library's record writes, one at a time.
        writes: Writes,
        shelf: Option<LibraryWindow>,
        sheet: Option<SheetWindow<Sheet>>,
        /// The menu up over a card of the library, its machine, and the rows
        /// it was last given.
        card_menu: Option<Followed<u32>>,
        /// The deletion asked about: its alert and its machine.
        asking: Option<(u64, u32)>,
        machines: Vec<MachineWindow>,
        /// The workers the picture tube spreads across: none until an effect
        /// first draws.
        pool: LazyPool,
        /// Refusals met while the windows were being served, said once it is
        /// done.
        notes: Vec<(String, String)>,
        /// The last start's ticket.
        tickets: u64,
        pictures: Pictures,
        /// The machines whose state in the store holds a snapshot.
        suspended: Vec<u32>,
        /// The machines suspended whose state is not yet kept, each holding
        /// its window open until it is.
        keeping: Suspends<WindowPane>,
        /// When leaving must end, however much is still being kept.
        leaving: Option<u64>,
        quit: bool,
    }

    impl App {
        /// The window an alert about the library is held over.
        fn owner(&self) -> Option<u64> {
            self.shelf.as_ref().map(LibraryWindow::id)
        }

        /// Tell the user `headline` was not done, and why, over `owner`.
        fn refuse_over(&mut self, owner: Option<u64>, headline: &str, why: &str) {
            let _ = app::refuse(
                &mut self.client,
                (APP_NAME, self.event_endpoint),
                owner,
                headline,
                why,
            );
        }

        fn refuse(&mut self, headline: &str, why: &str) {
            let owner = self.owner();
            self.refuse_over(owner, headline, why);
        }

        /// Tell the user something partly went wrong, over window `owner`:
        /// `detail` is a whole sentence.
        fn warn_over(&mut self, owner: Option<u64>, headline: &str, detail: &str) {
            app::report(APP_NAME, format_args!("{headline}: {detail}"));
            let raised = AlertSpec::acknowledged(AlertSeverity::Warning, headline, detail)
                .and_then(|alert| self.client.open_alert(self.event_endpoint, owner, &alert));
            if let Err(err) = raised {
                app::report(
                    APP_NAME,
                    format_args!("the desktop would not show that ({err})"),
                );
            }
        }

        /// Ask the library worker for `job`: false, said, where it holds as
        /// many as it can.
        fn ask(&mut self, job: Job) -> bool {
            let taken = self.store.submit(job).is_ok();
            if !taken {
                app::report(APP_NAME, "too much is waiting on the store");
            }
            taken
        }

        /// The library changed: write it, or again once the write under way
        /// lands.
        fn save(&mut self) {
            if self.shelving != Shelving::Read || !self.writes.begin() {
                return;
            }
            match self.library.record() {
                Ok(record) => {
                    if !self.ask(Job::Record(record)) {
                        self.recorded(Err(String::from("the store is too busy")));
                    }
                }
                Err(err) => self.recorded(Err(format!("its record could not be made ({err:?})"))),
            }
        }

        /// A record write landed: what waited on it goes, or waits for the
        /// next.
        fn recorded(&mut self, result: Result<u64, String>) {
            let landed = self.writes.landed(result.is_ok());
            match result {
                Ok(generation) => {
                    self.library.written(generation);
                    for id in landed.removals {
                        self.ask(Job::Remove { id });
                    }
                }
                Err(why) if landed.newly_failed => {
                    self.refuse("The machine library could not be saved", &why);
                }
                Err(_) => {}
            }
            if landed.again {
                self.save();
            }
        }

        /// Adopt what the workers landed.
        fn collect(&mut self) {
            let mut landed = Vec::new();
            self.store.collect_landed(|done| landed.push(done));
            for done in landed {
                self.adopt(done);
            }
            let mut served = Vec::new();
            self.disks.collect_landed(|done| served.push(done));
            for done in served {
                self.served(done);
            }
            let mut compiled = Vec::new();
            if let Some(blocks) = &self.blocks {
                blocks.relay.collect_landed(|done| compiled.push(done));
            }
            for done in compiled {
                self.compiled(done);
            }
        }

        /// Adopt what the compile worker answered.
        fn compiled(&mut self, done: CompileDone) {
            let (machine, request, code) = match done {
                CompileDone::Compiled {
                    machine,
                    request,
                    code,
                } => (machine, request, code),
                CompileDone::Interpret { machine, request } => {
                    if let Some(held) = self.machines.iter_mut().find(|held| held.ticket == machine)
                    {
                        app::report(
                            APP_NAME,
                            format_args!(
                                "\u{201c}{}\u{201d} is interpreted from now on: its compiler keeps failing",
                                held.name
                            ),
                        );
                    }
                    if let Some(run) = self.running_start(machine) {
                        run.host.interpret();
                    }
                    (machine, request, None)
                }
                CompileDone::Forgot { machine } => {
                    self.retiring.retain(|(held, _)| *held != machine);
                    return;
                }
            };
            if let Some(run) = self.running_start(machine) {
                let _ = run.host.answer_block(request, code);
            }
        }

        /// The session of the machine started as `ticket`, while it runs.
        fn running_start(&mut self, ticket: u64) -> Option<&mut Running> {
            self.machines
                .iter_mut()
                .find(|machine| machine.ticket == ticket)?
                .run
                .as_mut()
        }

        fn adopt(&mut self, done: Done) {
            match done {
                Done::Loaded(shelved) => self.shelved(shelved),
                Done::Recorded(result) => self.recorded(result),
                Done::Made { id, disk } => self.made(id, disk),
                Done::Copied { from, to, result } => self.copied(from, to, result),
                Done::Removed(result) => {
                    if let Err(why) = result {
                        app::report(APP_NAME, format_args!("a machine's files stay ({why})"));
                    }
                }
                Done::Kept {
                    id,
                    picture,
                    why,
                    result,
                } => {
                    if let Some(picture) = picture.filter(|_| self.library.machine(id).is_some()) {
                        self.pictures.put(id, &picture);
                        self.picture_drawn(id);
                    }
                    self.state_kept(id, why, result);
                }
                Done::Pictured(pictures) => {
                    for (id, picture) in pictures {
                        self.pictures.put(id, &picture);
                        self.picture_drawn(id);
                    }
                }
                Done::Shot(result) => {
                    if let Err(why) = result {
                        self.refuse("The screenshot could not be kept", &why);
                    }
                }
                Done::Started {
                    id,
                    ticket,
                    launched,
                } => self.started(id, ticket, launched),
            }
        }

        /// The library was read: show it, or make the first machine.
        fn shelved(&mut self, shelved: Result<Shelved<Machine>, String>) {
            let Shelved {
                loaded,
                pictures,
                suspended,
                unswept,
            } = match shelved {
                Ok(shelved) => shelved,
                Err(why) => {
                    self.shelving = Shelving::Unread;
                    self.refuse("The machine library could not be read", &why);
                    return;
                }
            };
            self.library = loaded.library;
            self.suspended = suspended;
            self.shelving = Shelving::Read;
            for (id, picture) in pictures {
                self.pictures.put(id, &picture);
            }
            if unswept > 0 {
                app::report(
                    APP_NAME,
                    format_args!("{unswept} files of machines no longer kept could not be removed"),
                );
            }
            match loaded.found {
                Found::Fresh => {
                    let (template, name) = assistant::TEMPLATES[assistant::OFFERED];
                    self.create(template, name, true);
                }
                Found::Unreadable => self.refuse(
                    "The machine library could not be read",
                    "Neither copy of it checks out, so the library starts empty.",
                ),
                Found::Read => {}
            }
            self.show();
        }

        fn made(&mut self, id: u32, disk: Result<StoredDisk, String>) {
            let Some(at) = self.pending.iter().position(|machine| machine.id == id) else {
                return;
            };
            let mut machine = self.pending.remove(at);
            match disk {
                Ok(disk) => {
                    machine.disks[0] = Some(disk);
                    self.adopt_new(machine);
                }
                Err(why) => {
                    self.ask(Job::Remove { id });
                    self.refuse(
                        &format!("\u{201c}{}\u{201d} could not be made", machine.name),
                        &why,
                    );
                }
            }
        }

        fn copied(&mut self, from: u32, to: u32, result: Result<(), String>) {
            let Some(at) = self.pending.iter().position(|machine| machine.id == to) else {
                return;
            };
            let machine = self.pending.remove(at);
            match result {
                Ok(()) => {
                    if self.suspended.contains(&from) && self.suspended.try_reserve(1).is_ok() {
                        self.suspended.push(to);
                    }
                    self.pictures.copy(from, to);
                    self.adopt_new(machine);
                }
                Err(why) => {
                    self.ask(Job::Remove { id: to });
                    self.refuse(
                        &format!("\u{201c}{}\u{201d} could not be made", machine.name),
                        &why,
                    );
                }
            }
        }

        /// A machine whose disks are made joins the library.
        fn adopt_new(&mut self, machine: Machine) {
            let (id, headline) = (
                machine.id,
                format!("\u{201c}{}\u{201d} could not be added", machine.name),
            );
            match self.library.add(machine) {
                Ok(()) => {
                    self.save();
                    self.show();
                }
                Err(err) => {
                    self.ask(Job::Remove { id });
                    self.refuse(&headline, &format!("the library refused it ({err:?})"));
                }
            }
        }

        /// Adopt what the disk worker answered.
        fn served(&mut self, done: DiskDone) {
            match done {
                DiskDone::Attached { id, held } => {
                    if !held {
                        if let Some(n) = self.machine_index(id) {
                            self.stop(
                                n,
                                Some(String::from("there is no memory to serve its disks")),
                            );
                        }
                    }
                }
                DiskDone::Forgot => {}
                DiskDone::Taken { id, unit, taken } => self.taken(id, unit, taken),
                DiskDone::Fetched { id, answer } => {
                    if let Some(run) = self.running(id) {
                        let _ = run.host.answer_fetch(answer.as_deref().map_err(|err| *err));
                    }
                }
                DiskDone::Stored { id, answer } => {
                    if let Some(run) = self.running(id) {
                        let _ = run.host.answer_store(answer);
                    }
                }
                DiskDone::Synced { id, result } => self.synced(id, result),
                DiskDone::Shared { id, slot, result } => self.shared(id, slot, result),
                DiskDone::Foldered { id, answer } => {
                    if let Some(run) = self.running(id) {
                        let _ = run.host.answer_folder(
                            answer.as_ref().map(Foldered::reply).map_err(|why| *why),
                        );
                    }
                }
            }
        }

        fn machine_index(&self, id: u32) -> Option<usize> {
            self.machines.iter().position(|machine| machine.id == id)
        }

        fn running(&mut self, id: u32) -> Option<&mut Running> {
            self.machines
                .iter_mut()
                .find(|machine| machine.id == id)?
                .run
                .as_mut()
        }

        /// Open the library window, or raise the one open.
        fn open_shelf(&mut self) {
            if let Some(shelf) = &self.shelf {
                let _ = self.client.activate_window(shelf.id());
                return;
            }
            let look = LibraryLook::titled(APP_TITLE);
            let looks = (&self.desktop, self.themes.active());
            match LibraryWindow::open(&mut self.client, self.event_endpoint, looks, look) {
                Ok(win) => {
                    self.shelf = Some(win);
                    self.show();
                    self.paint_whole_shelf();
                }
                Err(why) => app::report(APP_NAME, why),
            }
        }

        /// Show the library as it stands, repainting what that changed.
        fn show(&mut self) {
            let (scale, theme) = (self.desktop.scale(), self.themes.active());
            let Some(win) = &mut self.shelf else {
                return;
            };
            let held = (
                self.machines.as_slice(),
                &self.keeping,
                self.suspended.as_slice(),
            );
            let entries = self.library.machines().iter().map(|machine| Entry {
                id: machine.id,
                name: machine.name.clone(),
                about: library::describe(machine),
                standing: standing_of(held, machine.id),
            });
            win.show(entries, (scale, theme), &self.pictures);
        }

        /// Repaint the whole library window: its size or its look changed.
        fn paint_whole_shelf(&mut self) {
            let looks = (self.desktop.scale(), self.themes.active());
            if let Some(win) = &mut self.shelf {
                win.paint_whole(looks, &self.pictures);
            }
        }

        /// Machine `id`'s picture was drawn anew: repaint its card.
        fn picture_drawn(&mut self, id: u32) {
            let looks = (self.desktop.scale(), self.themes.active());
            if let Some(win) = &mut self.shelf {
                win.picture_drawn(id, looks, &self.pictures);
            }
        }

        fn shelf_event(&mut self, event: &WindowEvent) {
            let looks = (self.desktop.scale(), self.themes.active());
            let interval = self.desktop.info().double_click();
            let Some(win) = &mut self.shelf else {
                return;
            };
            match win.event(&mut self.client, event, (looks, interval), &self.pictures) {
                LibraryEvent::Nothing => {}
                LibraryEvent::Closed => self.close_shelf(),
                LibraryEvent::Menu {
                    open_id,
                    item,
                    kept,
                } => self.card_answered(open_id, item, kept),
                LibraryEvent::Request(request) => self.request(request),
            }
        }

        /// Step the library's held scrollbar press, once it is due.
        fn step_shelf(&mut self) {
            let looks = (self.desktop.scale(), self.themes.active());
            if let Some(win) = &mut self.shelf {
                win.step(tairix_rt::clock_get(), looks, &self.pictures);
            }
        }

        /// The library window closed, leaving the program on the icon bar.
        fn close_shelf(&mut self) {
            self.close_sheet();
            self.card_menu = None;
            if let Some(win) = self.shelf.take() {
                win.close(&mut self.client);
            }
            self.end_once_left();
        }

        fn request(&mut self, request: Request) {
            match self.shelving {
                Shelving::Read => {}
                Shelving::Reading => {
                    self.refuse(
                        "The machine library is still being read",
                        "Try again in a moment.",
                    );
                    return;
                }
                Shelving::Unread => {
                    self.refuse(
                        "The machine library could not be read",
                        "No machine can be made or changed until it is.",
                    );
                    return;
                }
            }
            match request {
                Request::New => self.open_assistant(),
                Request::Start(id) => self.start(id),
                Request::Settings(id) => self.open_settings(id),
                Request::Duplicate(id) => self.duplicate(id),
                Request::Delete(id) => self.ask_delete(id),
                Request::Menu { id, at } => self.open_card_menu(id, at),
            }
        }

        /// The rows of machine `id`'s card menu as it stands now.
        fn card_rows(&self, id: u32) -> Option<AppMenu> {
            let running = self
                .machine_index(id)
                .map(|n| &self.machines[n])
                .filter(|machine| machine.life == Life::Running)
                .map(MachineWindow::shown);
            let rows = running
                .as_ref()
                .map(|shown| move |menu: &mut AppMenu| machine_menu::push_machine(menu, shown));
            card_menu(self.standing(id), rows).ok()
        }

        /// Open machine `id`'s card menu at `at` in the library window.
        fn open_card_menu(&mut self, id: u32, at: tairix_geometry::Point) {
            let (Some(rows), Some(win)) = (self.card_rows(id), &self.shelf) else {
                return;
            };
            match win.open_menu(&mut self.client, at, &rows) {
                Ok(open) => self.card_menu = Some(Followed::new(open, id, rows)),
                Err(err) => app::report(
                    APP_NAME,
                    format_args!("the menu could not be opened ({err})"),
                ),
            }
        }

        /// Row `item` of the card menu `open_id` was chosen, with the menu
        /// `kept` up or ending with it; with no row it ended without one.
        fn card_answered(&mut self, open_id: u64, item: Option<AppMenuItemId>, kept: bool) {
            let Some(id) = self
                .card_menu
                .as_ref()
                .filter(|card| card.open() == open_id)
                .map(Followed::kind)
            else {
                return;
            };
            if !kept {
                self.card_menu = None;
            }
            match item.map(card_command) {
                None => {}
                Some(CardCommand::Start) => self.request(Request::Start(id)),
                Some(CardCommand::Settings) => self.request(Request::Settings(id)),
                Some(CardCommand::Duplicate) => self.request(Request::Duplicate(id)),
                Some(CardCommand::Delete) => self.request(Request::Delete(id)),
                Some(CardCommand::Machine(item)) => {
                    let command = machine_menu::command(item);
                    if let (Some(n), Some(command)) = (self.machine_index(id), command) {
                        self.machine_command(n, command);
                    }
                }
            }
        }

        /// Show every menu up as what it is for now stands, once the events
        /// of a pass are in: a row chosen with it kept up, or the machine
        /// changing under it — a diskette ejected, a paste ended, the window
        /// filling the display — restates exactly the rows that moved.
        fn follow_menus(&mut self) {
            for machine in &mut self.machines {
                let rows = machine
                    .menu
                    .as_ref()
                    .and_then(|up| machine.menu_of(up.kind()));
                let (Some(up), Some(rows)) = (machine.menu.as_mut(), rows) else {
                    continue;
                };
                let open = up.open();
                if let Some(rows) = up.follow(rows) {
                    library_window::restate(
                        &mut self.client,
                        (APP_NAME, machine.window.pane.id()),
                        open,
                        rows,
                    );
                }
            }
            let Some(window_id) = self.shelf.as_ref().map(LibraryWindow::id) else {
                self.card_menu = None;
                return;
            };
            let rows = self
                .card_menu
                .as_ref()
                .and_then(|card| self.card_rows(card.kind()));
            if let (Some(card), Some(rows)) = (self.card_menu.as_mut(), rows) {
                let open = card.open();
                if let Some(rows) = card.follow(rows) {
                    library_window::restate(&mut self.client, (APP_NAME, window_id), open, rows);
                }
            }
        }

        fn open_assistant(&mut self) {
            let names = assistant::TEMPLATES
                .iter()
                .map(|(_, name)| self.library.fresh_name(name))
                .collect();
            self.open_sheet(
                Sheet::New(Box::new(assistant::assistant(names))),
                assistance::TITLE,
            );
        }

        fn open_settings(&mut self, id: u32) {
            let Some(machine) = self.library.machine(id) else {
                return;
            };
            let settings = Box::new(Settings::new(machine, self.standing(id)));
            self.open_sheet(Sheet::Settings { id, settings }, settings::TITLE);
        }

        /// Open `sheet` as an owner-modal dialog over the library window.
        fn open_sheet(&mut self, sheet: Sheet, title: &str) {
            let Some(owner) = self.owner() else {
                return;
            };
            if self.sheet.is_some() {
                return;
            }
            let looks = (self.desktop.scale(), self.themes.active());
            let at = (owner, self.event_endpoint);
            match SheetWindow::open(&mut self.client, at, looks, sheet, title) {
                Ok(win) => self.sheet = Some(win),
                Err(why) => self.refuse(&format!("{title} could not be opened"), &why),
            }
        }

        fn close_sheet(&mut self) {
            if let Some(win) = self.sheet.take() {
                win.close(&mut self.client);
            }
        }

        fn sheet_event(&mut self, event: &WindowEvent) {
            let looks = (self.desktop.scale(), self.themes.active());
            let Some(win) = &mut self.sheet else {
                return;
            };
            match win.event(event, looks) {
                SheetEvent::Nothing => {}
                SheetEvent::Closed => self.close_sheet(),
                SheetEvent::Answered(answer) => {
                    self.close_sheet();
                    self.answered(answer);
                }
            }
        }

        fn answered(&mut self, answer: SheetAnswer) {
            match answer {
                SheetAnswer::New(assistance::Answer::Create {
                    model,
                    name,
                    choice,
                }) => {
                    if let Some((template, disk)) = assistant::chosen(model, choice) {
                        self.create(template, &name, disk);
                    }
                }
                SheetAnswer::Settings(id, settings::Answer::Apply(choices)) => {
                    let Some(mut machine) = self.library.machine(id).cloned() else {
                        return;
                    };
                    if let Some(why) = self.standing(id).locked() {
                        self.refuse("The settings could not be applied", why);
                        return;
                    }
                    machine.config = choices.configured(&machine.config);
                    machine.name.clone_from(&choices.name);
                    match self.library.replace(machine) {
                        Ok(()) => {
                            self.save();
                            self.show();
                        }
                        Err(err) => self.refuse(
                            "The settings could not be applied",
                            &format!("the library refused them ({err:?})"),
                        ),
                    }
                }
                SheetAnswer::New(assistance::Answer::Cancel)
                | SheetAnswer::Settings(_, settings::Answer::Cancel) => {}
            }
        }

        /// Make a `template` machine called `name`, with its default disk
        /// where `disk`.
        fn create(&mut self, template: Template, name: &str, disk: bool) {
            let id = match self.library.mint() {
                Ok(id) => id,
                Err(err) => {
                    self.refuse(
                        "No machine can be made",
                        &format!("the library is full ({err:?})"),
                    );
                    return;
                }
            };
            let config = template.config();
            let recording = config.fixed.map(|adapter| adapter.recording);
            let machine = Machine {
                id,
                name: String::from(name),
                config,
                disks: [None, None],
                clock_offset: 0,
                display: tairix_crt::Tube::default(),
            };
            match recording.filter(|_| disk) {
                Some(recording) => {
                    let mut serial = [0u8; 4];
                    let _ = tairix_rt::random_fill(&mut serial);
                    if self.pending.try_reserve(1).is_err() {
                        self.refuse(
                            &format!("\u{201c}{name}\u{201d} could not be made"),
                            "there is not enough memory",
                        );
                        return;
                    }
                    self.pending.push(machine);
                    let job = Job::Make {
                        id,
                        recording,
                        serial: u32::from_le_bytes(serial),
                    };
                    if !self.ask(job) {
                        self.made(id, Err(String::from("the store is too busy")));
                    }
                }
                None => self.adopt_new(machine),
            }
        }

        /// Copy machine `id`, its disks and its last screen with it.
        fn duplicate(&mut self, id: u32) {
            let Some(source) = self.library.machine(id).cloned() else {
                return;
            };
            if !self.standing(id).at_rest() {
                return;
            }
            let to = match self.library.mint() {
                Ok(to) => to,
                Err(err) => {
                    self.refuse(
                        "No machine can be made",
                        &format!("the library is full ({err:?})"),
                    );
                    return;
                }
            };
            let drives = [source.disks[0].is_some(), source.disks[1].is_some()];
            let copy = Machine {
                id: to,
                name: self.library.copy_name(&source.name),
                ..source
            };
            if self.pending.try_reserve(1).is_err() {
                self.refuse(
                    &format!("\u{201c}{}\u{201d} could not be made", copy.name),
                    "there is not enough memory",
                );
                return;
            }
            self.pending.push(copy);
            if !self.ask(Job::Copy {
                from: id,
                to,
                drives,
            }) {
                self.copied(id, to, Err(String::from("the store is too busy")));
            }
        }

        /// Ask whether machine `id` should be deleted.
        fn ask_delete(&mut self, id: u32) {
            let Some(machine) = self.library.machine(id) else {
                return;
            };
            let Some(question) = library_window::delete_question(
                &machine.name,
                "Its hard disks and everything on them are deleted for good.",
            ) else {
                return;
            };
            if let Some((alert, _)) = self.asking.take() {
                let _ = self.client.close_alert(alert);
            }
            let owner = self.owner();
            match self
                .client
                .open_alert(self.event_endpoint, owner, &question)
            {
                Ok(alert) => self.asking = Some((alert, id)),
                Err(err) => app::report(
                    APP_NAME,
                    format_args!("the desktop would not ask about the deletion ({err})"),
                ),
            }
        }

        /// Delete machine `id`: out of the library at once, its files once a
        /// record without it is kept.
        fn delete(&mut self, id: u32) {
            if !self.standing(id).at_rest() || self.library.remove(id).is_none() {
                return;
            }
            self.pictures.forget(id);
            self.suspended.retain(|&held| held != id);
            self.writes.doom(id);
            self.save();
            self.show();
        }

        /// Start machine `id`, resuming it where it is suspended, or raise
        /// its window.
        fn start(&mut self, id: u32) {
            if !self.standing(id).startable() {
                return;
            }
            if let Some(n) = self.machine_index(id) {
                let _ = self
                    .client
                    .activate_window(self.machines[n].window.pane.id());
                return;
            }
            let Some(machine) = self.library.machine(id) else {
                return;
            };
            let name = machine.name.clone();
            let sectors = [machine.disk_sectors(0), machine.disk_sectors(1)];
            let share = Layout::of(&machine.config.video).len();
            let bar = MachineBar::new(&machine.config, sectors.map(|disk| disk.is_some()));
            let (tube, speed) = (machine.display, machine.config.clock);
            let Some(slot) = (0..Machine::MAX_MACHINES)
                .find(|slot| self.machines.iter().all(|machine| machine.slot != *slot))
            else {
                self.refuse(NOT_STARTED, "as many machines as can run are running");
                return;
            };
            let drawing = drawing(&self.pool, &self.desktop);
            let window = match open_machine_window(
                &mut self.client,
                (self.event_endpoint, &name, bar, tube),
                (&self.desktop, self.themes.active(), drawing),
            ) {
                Ok(window) => window,
                Err(why) => {
                    self.refuse(NOT_STARTED, &why);
                    return;
                }
            };
            if self.machines.try_reserve(1).is_err() {
                let _ = window.pane.close(&mut self.client);
                self.refuse(NOT_STARTED, "there is not enough memory");
                return;
            }
            self.tickets += 1;
            let ticket = self.tickets;
            self.machines.push(MachineWindow {
                id,
                life: Life::Starting,
                resumed: None,
                ran: false,
                answer_by: None,
                ticket,
                name,
                slot,
                window,
                run: None,
                sound: None,
                menu: None,
                picking: None,
                naming: None,
                sharing: None,
                shares: [const { None }; SHARES],
                held: Default::default(),
                coming: Default::default(),
                debugger: None,
                paused: false,
                speed,
                fullscreen: false,
                clipboard: Clipboard::default(),
            });
            if !self.ask(Job::Start {
                id,
                ticket,
                sectors,
                share,
                suspended: self.suspended.contains(&id),
            }) {
                if let Some(n) = self.machine_index(id) {
                    self.stop(n, Some(String::from("the store is too busy")));
                }
            }
            self.show();
        }

        /// Machine `id`'s disks, worker and sound, or why there are none:
        /// run it.
        fn started(&mut self, id: u32, ticket: u64, launched: Result<Launched, String>) {
            let current = self
                .machine_index(id)
                .filter(|n| self.machines[*n].ticket == ticket);
            let Some(n) = current else {
                // Its window closed while it was starting.
                if let Ok(launched) = launched {
                    if let Some(sound) = launched.sound {
                        sound.close();
                    }
                }
                return;
            };
            let launched = match launched {
                Ok(launched) => launched,
                Err(why) => {
                    self.stop(n, Some(why));
                    return;
                }
            };
            let Some(machine) = self.library.machine(id).cloned() else {
                self.stop(n, Some(String::from("it is no longer in the library")));
                return;
            };
            let Launched {
                disks,
                session,
                sound,
                resume,
            } = launched;
            let sectors = [machine.disk_sectors(0), machine.disk_sectors(1)];
            let [read, write, sounding] = machine_tokens(self.machines[n].slot);
            let rate = sound.as_ref().map_or(SOUND_RATE, |opened| opened.rate);
            if let Some(mut opened) = sound {
                let watched = tairix_rt::waitset_ctl(
                    self.set,
                    WaitSetOp::Add,
                    WaitSourceKind::Port,
                    opened.stream.notify_port(),
                    sounding,
                );
                if watched < 0 {
                    let err = Errno::from_syscall(watched);
                    app::report(
                        APP_NAME,
                        format_args!(
                            "the sound's wake was refused ({err}); it plays as frames arrive"
                        ),
                    );
                }
                let _ = opened.stream.take_notify(&mut opened.audio);
                self.machines[n].sound = Some((opened, Sound::new(SOUND_WAITING_FRAMES)));
            }
            for (drive, file) in disks.into_iter().enumerate() {
                let Some(file) = file else { continue };
                let job = DiskJob::Attach {
                    id,
                    medium: medium_of(drive),
                    file: Some(file),
                };
                if self.disks.submit(job).is_err() {
                    self.stop(n, Some(String::from("too much is waiting on its disks")));
                    return;
                }
            }
            let Some(setup) = setup(&machine, sectors, rate) else {
                self.stop(
                    n,
                    Some(String::from(
                        "the system has no secure random bytes to key its disk cache with",
                    )),
                );
                return;
            };
            let worker = session.pid();
            match self.open_host(n, (session, setup), resume) {
                Ok((mut host, opening)) => {
                    if self.blocks.is_none() || worker.is_none() {
                        host.interpret();
                    }
                    let machine = &mut self.machines[n];
                    machine.life = Life::Opening(opening);
                    machine.run = Some(Running {
                        host,
                        members: SessionMembers::new(self.set, read, write),
                        worker,
                    });
                }
                Err(err) => self.stop(n, Some(format!("its session was refused ({err:?})"))),
            }
        }

        /// Machine `n`'s host, over `session`, for `setup`: resuming the
        /// snapshot `resume` holds, its window showing the screen it was
        /// suspended with, or built afresh.
        fn open_host(
            &mut self,
            n: usize,
            (session, setup): (RtSessionChannel, Setup),
            resume: Resumable,
        ) -> Result<(Host<RtSessionChannel, LogSink>, Opening), StartError> {
            match resume {
                Resumable::Kept(held) => {
                    let (screen, suspended) = *held;
                    let away = suspended
                        .at
                        .zip(kept::calendar_secs())
                        .and_then(|(then, now)| u64::try_from(now.saturating_sub(then)).ok())
                        .map_or(0, |secs| secs.saturating_mul(NANOS_PER_SECOND));
                    let drawing = drawing(&self.pool, &self.desktop);
                    let window = &mut self.machines[n].window;
                    if window
                        .picture
                        .take(screen.width, screen.height, &screen.pixels)
                    {
                        window.draw(drawing);
                    }
                    Host::resume(session, LogSink, setup, suspended.kept, away)
                        .map(|host| (host, Opening::Resuming))
                }
                Resumable::Damaged => {
                    let machine = &self.machines[n];
                    let id = machine.id;
                    let headline = format!("\u{201c}{}\u{201d} starts afresh", machine.name);
                    self.suspended.retain(|&held| held != id);
                    self.notes.push((
                        headline,
                        String::from("Its saved state does not check out."),
                    ));
                    Host::start(session, LogSink, setup).map(|host| (host, Opening::Fresh))
                }
                Resumable::Nothing => {
                    Host::start(session, LogSink, setup).map(|host| (host, Opening::Fresh))
                }
            }
        }

        /// Turn machine `n` off and close its window, keeping its last screen
        /// once it has run; `why` it stopped, when it was not asked to.
        fn stop(&mut self, n: usize, why: Option<String>) {
            let Some(Retired {
                id,
                name,
                ran,
                screen,
                pane,
            }) = self.retire(n)
            else {
                return;
            };
            let _ = pane.close(&mut self.client);
            if let Some(screen) = screen.filter(|_| ran) {
                let picture = self.pictures.size();
                self.ask(Job::Keep {
                    id,
                    screen,
                    suspended: None,
                    picture,
                    why: Keeping::Off,
                });
            }
            if let Some(why) = why {
                let headline = format!("\u{201c}{name}\u{201d} stopped");
                self.refuse(&headline, &why);
            }
            self.show();
            self.end_once_left();
        }

        /// Take machine `n` out of the running: its session and sound closed
        /// and its images let go, its window handed back open.
        fn retire(&mut self, n: usize) -> Option<Retired> {
            if n >= self.machines.len() {
                return None;
            }
            self.close_debugger(n);
            let machine = self.machines.remove(n);
            let MachineWindow {
                id,
                ticket,
                name,
                slot,
                window,
                run,
                sound,
                ran,
                ..
            } = machine;
            if let Some(mut run) = run {
                let _ = run.members.sync(None, false, false);
                self.let_go(ticket, run);
            }
            if let Some((opened, _)) = sound {
                let [_, _, sounding] = machine_tokens(slot);
                unwatch_sound(self.set, &opened, sounding);
                opened.close();
            }
            let _ = self.disks.submit(DiskJob::Forget { id, medium: None });
            let screen = LastScreen::of(window.picture.screen());
            Some(Retired {
                id,
                name,
                ran,
                screen,
                pane: window.pane,
            })
        }

        /// Let machine `ticket`'s session go: at once, or once the compile
        /// worker has forgotten the machine, its worker ended meanwhile.
        fn let_go(&mut self, ticket: u64, run: Running) {
            let Some(blocks) = &self.blocks else {
                return;
            };
            if blocks
                .relay
                .submit(CompileJob::Forget { machine: ticket })
                .is_err()
            {
                return;
            }
            if let Some(worker) = run.worker {
                let _ = tairix_rt::signal(worker, tairix_abi::Signal::Kill);
            }
            if self.retiring.try_reserve(1).is_ok() {
                self.retiring.push((ticket, run.host));
            }
        }

        /// Machine `n`'s window asked to close: a machine with a worker is
        /// suspended, its window closing once its snapshot has come, and one
        /// still opening its disks is turned off.
        fn close(&mut self, n: usize) {
            if self.machines[n].life == Life::Suspending {
                return;
            }
            self.close_debugger(n);
            let looks = (self.desktop.scale(), self.themes.active());
            let machine = &mut self.machines[n];
            let asked = machine
                .run
                .as_mut()
                .is_some_and(|run| run.host.suspend().is_ok());
            if !asked {
                self.stop(n, None);
                return;
            }
            machine.life = Life::Suspending;
            machine.answer_by = Some(tairix_rt::clock_get().saturating_add(SUSPEND_GRACE_NS));
            let mut damage = Region::new();
            machine.window.bar.set_saving(&mut damage);
            machine.window.paint_bar(&damage, looks);
        }

        /// Machine `n` stopped and sent `kept`, its snapshot, which is kept
        /// once what it wrote to its images is durable, so it is never
        /// resumed against disks older than it; its window, saying it is
        /// saving, closes once the snapshot is kept.
        fn suspended_whole(&mut self, n: usize, kept: Vec<u8>) {
            let id = self.machines[n].id;
            let synced = self.disks.submit(DiskJob::Sync { id }).is_ok();
            let Some(Retired {
                id,
                name,
                screen,
                pane,
                ..
            }) = self.retire(n)
            else {
                return;
            };
            let Some(screen) = screen else {
                let _ = pane.close(&mut self.client);
                self.notes.push((
                    format!("\u{201c}{name}\u{201d} could not be suspended"),
                    String::from("It had shown nothing to keep, so it was turned off."),
                ));
                self.show();
                self.end_once_left();
                return;
            };
            let suspended = Suspended {
                kept,
                at: kept::calendar_secs(),
            };
            if !synced || !self.keeping.reserve() {
                let _ = pane.close(&mut self.client);
                self.notes.push((
                    format!("\u{201c}{name}\u{201d} could not be suspended"),
                    String::from("Too much is waiting on the disks, so it was turned off."),
                ));
                let picture = self.pictures.size();
                self.ask(Job::Keep {
                    id,
                    screen,
                    suspended: None,
                    picture,
                    why: Keeping::Off,
                });
            } else {
                self.keeping.suspend((id, name), (screen, suspended), pane);
            }
            self.show();
            self.end_once_left();
        }

        /// Machine `id`'s images are durable, or why not: its snapshot goes
        /// to be kept, or, where they are not, only its last screen.
        fn synced(&mut self, id: u32, result: Result<(), String>) {
            let picture = self.pictures.size();
            let Some((name, screen, suspended)) = self.keeping.synced(id, result.is_ok()) else {
                return;
            };
            if let Err(why) = result {
                self.notes.push((
                    format!("\u{201c}{name}\u{201d} could not be suspended"),
                    format!("Its disk could not be kept ({why}), so it was turned off."),
                ));
            }
            if !self.ask(Job::Keep {
                id,
                screen,
                suspended,
                picture,
                why: Keeping::Suspended,
            }) {
                if let Some(ended) = self.keeping.end(id) {
                    let _ = ended.window.close(&mut self.client);
                }
                self.end_once_left();
            }
        }

        /// What became of a keep of machine `id`'s state, kept for `why`.
        fn state_kept(&mut self, id: u32, why: Keeping, result: Result<(), String>) {
            match why {
                Keeping::Off => {
                    if let Err(why) = result {
                        app::report(APP_NAME, format_args!("{why}"));
                    }
                }
                Keeping::SetAside => self.set_aside(id, result),
                Keeping::Suspended => {
                    let Some(Ended {
                        name,
                        with_snapshot,
                        window,
                    }) = self.keeping.end(id)
                    else {
                        return;
                    };
                    let _ = window.close(&mut self.client);
                    match result {
                        Ok(()) if with_snapshot => {
                            if !self.suspended.contains(&id)
                                && self.suspended.try_reserve(1).is_ok()
                            {
                                self.suspended.push(id);
                            }
                        }
                        Ok(()) => {}
                        Err(why) => self.notes.push((
                            format!("\u{201c}{name}\u{201d} could not be suspended"),
                            why,
                        )),
                    }
                    self.show();
                    self.end_once_left();
                }
            }
        }

        /// Machine `id`'s snapshot is set aside, or why not: it runs, or is
        /// turned off where its snapshot might be resumed again from under
        /// what it would write.
        fn set_aside(&mut self, id: u32, result: Result<(), String>) {
            let Some(n) = self
                .machine_index(id)
                .filter(|&n| self.machines[n].life == Life::SettingAside)
            else {
                return;
            };
            if let Err(why) = result {
                self.stop(
                    n,
                    Some(format!(
                        "its saved state could not be set aside ({why}), so it was not resumed"
                    )),
                );
                return;
            }
            self.suspended.retain(|&held| held != id);
            let machine = &mut self.machines[n];
            let Some(run) = machine.run.as_mut() else {
                return;
            };
            if run.host.run().is_ok() {
                machine.ran = true;
            }
            machine.life = Life::Running;
            if machine.resumed == Some(false) {
                let headline = format!("\u{201c}{}\u{201d} starts afresh", machine.name);
                self.notes.push((
                    headline,
                    String::from("Its saved state could not be taken back."),
                ));
            }
            self.show();
        }

        /// End the program, once *Quit* has asked it to leave, as soon as
        /// nothing is left to it: no window and no machine still being kept.
        /// Closing its windows alone leaves it on the icon bar.
        fn end_once_left(&mut self) {
            if self.leaving.is_some()
                && self.machines.is_empty()
                && self.keeping.is_empty()
                && self.shelf.is_none()
            {
                self.quit = true;
            }
        }

        /// Leave: every machine with a worker suspended, every other turned
        /// off, and the program ended once they are kept, or at a deadline.
        fn leave(&mut self) {
            self.close_sheet();
            if let Some(win) = self.shelf.take() {
                win.close(&mut self.client);
            }
            for n in (0..self.machines.len()).rev() {
                self.close(n);
            }
            self.leaving = Some(tairix_rt::clock_get().saturating_add(DRAIN_NS));
            self.end_once_left();
        }

        /// Turn off every machine that was asked to suspend and has not sent
        /// its snapshot in time.
        fn give_up_suspends(&mut self) {
            let now = tairix_rt::clock_get();
            for n in (0..self.machines.len()).rev() {
                if self.machines[n].answer_by.is_some_and(|by| by <= now) {
                    self.stop(
                        n,
                        Some(String::from(
                            "it did not send its state as it suspended, so it was turned off",
                        )),
                    );
                }
            }
        }

        /// End the program once the deadline for leaving has passed.
        fn leave_if_due(&mut self) {
            if self
                .leaving
                .is_some_and(|deadline| deadline <= tairix_rt::clock_get())
            {
                self.quit = true;
            }
        }

        /// Where machine `id` stands, as the library shows it.
        fn standing(&self, id: u32) -> Standing {
            standing_of((&self.machines, &self.keeping, &self.suspended), id)
        }

        fn machine_event(&mut self, n: usize, event: &WindowEvent) {
            match *event {
                WindowEvent::CloseRequested { .. } => self.close(n),
                WindowEvent::MenuClosed {
                    open_id, outcome, ..
                } => self.menu_closed(n, open_id, outcome),
                WindowEvent::MenuChosen { open_id, item, .. } => {
                    let chosen = self.machines[n]
                        .menu
                        .as_ref()
                        .filter(|up| up.open() == open_id)
                        .map(Followed::kind);
                    if let Some(menu) = chosen {
                        self.bar_menu_command(n, menu, item);
                    }
                }
                WindowEvent::FilePicked {
                    window_id,
                    handle,
                    writable,
                } => self.picked(n, (window_id, handle, writable)),
                WindowEvent::SubtreePicked {
                    window_id,
                    handle,
                    writable,
                } => self.subtree_picked(n, (window_id, handle, writable)),
                WindowEvent::PickCancelled { .. } => self.machines[n].picking = None,
                WindowEvent::FileOffered {
                    window_id, serial, ..
                } => {
                    let takes = self.machines[n].window.bar.takes_diskettes();
                    let asked = if takes {
                        self.client.ask_offered_file(window_id, serial)
                    } else {
                        self.client.answer_file_offer(window_id, serial, false)
                    };
                    if let Err(err) = asked {
                        app::report(
                            APP_NAME,
                            format_args!("a file offer went unanswered ({err})"),
                        );
                    }
                }
                _ => self.window_event(n, event),
            }
        }

        fn window_event(&mut self, n: usize, event: &WindowEvent) {
            let Self {
                machines,
                client,
                desktop,
                themes,
                pool,
                ..
            } = self;
            let (looks, drawing) = ((desktop.scale(), themes.active()), drawing(pool, desktop));
            let machine = &mut machines[n];
            let shown = machine.shown();
            let (window, name) = (&mut machine.window, machine.name.as_str());
            let host = machine.run.as_mut().map(|run| &mut run.host);
            match *event {
                WindowEvent::Resized {
                    width_px,
                    height_px,
                    state,
                    ..
                } => {
                    machine.fullscreen = state.is_fullscreen();
                    let mode = app::mode_for(width_px, height_px);
                    let (pane, surface) = (&mut window.pane, &mut window.surface);
                    if pane.resize_with(client, &mode, surface) == Fitted::Adopted {
                        window.redraw(looks, drawing);
                    }
                }
                WindowEvent::ContentReleased { .. } => {
                    window.pane.release_frames();
                    window.redraw(looks, drawing);
                }
                WindowEvent::RedrawRequested { .. } => window.owed.owe_whole(),
                WindowEvent::Key { key, .. } => {
                    if let Some(host) = host {
                        window.input.key(host, &key);
                    }
                }
                WindowEvent::Focus { focused, .. } => {
                    if let Some(host) = host {
                        let asks = window.input.focus(host, focused);
                        window.ask(client, asks, name);
                    }
                }
                WindowEvent::Pointer { x, y, action, .. } => {
                    let request = pointer(window, host, (x, y, action), looks);
                    window.ask(client, request.0, name);
                    if let Some(request) = request.1 {
                        machine.menu = open_bar_menu(client, window.pane.id(), request, &shown);
                    }
                }
                WindowEvent::PointerMotion { dx, dy, .. } => {
                    if let Some(host) = host {
                        window.input.motion(host, dx, dy);
                    }
                }
                WindowEvent::PointerLock { locked, .. } => {
                    if let Some(host) = host {
                        window.input.lock_changed(host, locked);
                    }
                    window.ask(client, Asks::default(), name);
                }
                _ => {}
            }
        }

        /// Machine `n`'s menu closed with `outcome`.
        fn menu_closed(&mut self, n: usize, open_id: u64, outcome: MenuOutcome) {
            let machine = &mut self.machines[n];
            let Some(menu) = machine
                .menu
                .take_if(|up| up.open() == open_id)
                .map(|up| up.kind())
            else {
                return;
            };
            if let MenuOutcome::Chosen(item) = outcome {
                self.bar_menu_command(n, menu, item);
            }
        }

        /// Do what row `item` of machine `n`'s `menu` says.
        fn bar_menu_command(&mut self, n: usize, menu: Menu, item: AppMenuItemId) {
            match menu {
                Menu::Machine => {
                    if let Some(command) = machine_menu::command(item) {
                        self.machine_command(n, command);
                    }
                }
                Menu::Drive { unit, .. } => self.drive_command(n, unit, item),
                Menu::Share { slot } => {
                    if item.get() == STOP_SHARING_ROW {
                        self.stop_sharing(n, slot);
                    }
                }
            }
        }

        /// Machine `n` shares the folder in `slot` no more: the machine drops
        /// its drive, the disk worker lets the folder go, and the bar forgets
        /// it.
        fn stop_sharing(&mut self, n: usize, slot: u8) {
            let looks = (self.desktop.scale(), self.themes.active());
            let machine = &mut self.machines[n];
            if machine
                .shares
                .get_mut(usize::from(slot))
                .and_then(Option::take)
                .is_none()
            {
                return;
            }
            if let Some(run) = &mut machine.run {
                if let Err(why) = run.host.unshare(slot) {
                    app::report(APP_NAME, format_args!("a share stays ({why:?})"));
                }
            }
            let id = machine.id;
            let mut damage = Region::new();
            machine.window.bar.set_share(slot, None, looks, &mut damage);
            machine.window.paint_bar(&damage, looks);
            if self.disks.submit(DiskJob::Unshare { id, slot }).is_err() {
                app::report(
                    APP_NAME,
                    "a folder no longer shared is held until its machine stops",
                );
            }
        }

        /// Do what row `item` of diskette drive `unit`'s menu says.
        fn drive_command(&mut self, n: usize, unit: u8, item: AppMenuItemId) {
            let machine = &mut self.machines[n];
            match item.get() {
                INSERT_ROW => {
                    let window_id = machine.window.pane.id();
                    match self.client.pick_file(window_id, PickPurpose::Open) {
                        Ok(()) => machine.picking = Some(Pick::Diskette(unit)),
                        Err(err) => app::report(
                            APP_NAME,
                            format_args!("the picker could not be asked for ({err})"),
                        ),
                    }
                }
                EJECT_ROW => {
                    if let Some(run) = &mut machine.run {
                        if let Err(why) = run.host.eject(unit) {
                            app::report(APP_NAME, format_args!("the disk stays in ({why:?})"));
                        }
                    }
                }
                _ => {}
            }
        }

        /// The picker chose a file for machine `n`: take it into a diskette
        /// drive and ask its name, or keep a screenshot in it.
        fn picked(&mut self, n: usize, (window_id, grant, writable): (u64, u64, bool)) {
            let machine = &mut self.machines[n];
            match machine.picking.take() {
                Some(Pick::Diskette(unit)) => {
                    if self.client.ask_picked_name(window_id).is_ok() {
                        machine.naming = Some(unit);
                    }
                    self.take_file(n, unit, (grant, writable), String::new());
                }
                Some(Pick::Screenshot(screen)) => {
                    if !self.ask(Job::Shoot { screen, grant }) {
                        self.refuse("The screenshot could not be kept", "the store is too busy");
                    }
                }
                Some(Pick::Share) | None => drop(File::from_delegation(grant)),
            }
        }

        /// The picker handed machine `n` a folder to share: once its name is
        /// known, the disk worker serves it.
        fn subtree_picked(&mut self, n: usize, (window_id, grant, writable): (u64, u64, bool)) {
            let machine = &mut self.machines[n];
            if !matches!(machine.picking.take(), Some(Pick::Share)) {
                drop(tairix_rt::Dir::from_delegation(grant));
                return;
            }
            let sharing = Sharing { grant, writable };
            if self.client.ask_picked_name(window_id).is_ok() {
                machine.sharing = Some(sharing);
            } else {
                self.share(n, sharing, "");
            }
        }

        /// Hand the folder `sharing` names, called `name`, to the disk worker
        /// to serve as machine `n`'s next share.
        fn share(&mut self, n: usize, sharing: Sharing, name: &str) {
            let machine = &mut self.machines[n];
            let id = machine.id;
            let Some(slot) = machine.shares.iter().position(Option::is_none) else {
                drop(tairix_rt::Dir::from_delegation(sharing.grant));
                self.refuse(
                    "The folder could not be shared",
                    "as many folders as a machine takes are shared",
                );
                return;
            };
            let slot = u8::try_from(slot).unwrap_or(u8::MAX);
            let label = alias::label(name);
            let offset = self.library.machine(id).map_or(0, |held| held.clock_offset);
            let job = DiskJob::Share {
                id,
                slot,
                grant: sharing.grant,
                writable: sharing.writable,
                label,
                offset,
            };
            if self.disks.submit(job).is_err() {
                drop(tairix_rt::Dir::from_delegation(sharing.grant));
                self.refuse("The folder could not be shared", "the disks are too busy");
                return;
            }
            self.machines[n].shares[usize::from(slot)] = Some(MachineShare {
                name: String::from(name),
                label,
                writable: sharing.writable,
                drive: None,
            });
        }

        /// The disk worker serves machine `id`'s share `slot`, or could not:
        /// the machine is told of it, and the bar shows it.
        fn shared(&mut self, id: u32, slot: u8, result: Result<(), String>) {
            let looks = (self.desktop.scale(), self.themes.active());
            let Some(n) = self.machine_index(id) else {
                let _ = self.disks.submit(DiskJob::Unshare { id, slot });
                return;
            };
            let machine = &mut self.machines[n];
            let index = usize::from(slot);
            let served = result.is_ok();
            let told = result.and_then(|()| match (&mut machine.run, &machine.shares[index]) {
                (Some(run), Some(share)) => run
                    .host
                    .share(slot, share.label, share.writable)
                    .map_err(|why| format!("the machine did not take it ({why:?})")),
                (None, _) => Err(String::from("the machine has stopped")),
                (_, None) => Err(String::from("it was stopped meanwhile")),
            });
            match told {
                Ok(()) => {
                    let Some(share) = &machine.shares[index] else {
                        return;
                    };
                    let mut damage = Region::new();
                    let shown = share.shown();
                    machine
                        .window
                        .bar
                        .set_share(slot, Some(shown), looks, &mut damage);
                    machine.window.paint_bar(&damage, looks);
                }
                Err(why) => {
                    machine.shares[index] = None;
                    if served {
                        let _ = self.disks.submit(DiskJob::Unshare { id, slot });
                    }
                    self.refuse("The folder could not be shared", &why);
                }
            }
        }

        /// Do what `command` of machine `n`'s menu says.
        fn machine_command(&mut self, n: usize, command: Command) {
            let drawing = drawing(&self.pool, &self.desktop);
            let looks = (self.desktop.scale(), self.themes.active());
            let machine = &mut self.machines[n];
            let window_id = machine.window.pane.id();
            // Only a machine running is run, held or reset: one still opening
            // or setting its snapshot aside is not to be run before it has.
            let live = machine.life == Life::Running;
            let mut shut = false;
            let refused = match (command, machine.run.as_mut().filter(|_| live)) {
                (Command::ShutDown, _) => {
                    shut = true;
                    None
                }
                // Opened once the menu's hold on the machine is let go.
                (Command::Debugger, _) => None,
                (Command::Effect(effect), _) => {
                    let tube = effect.toggled(machine.window.picture.tube());
                    machine.window.set_tube(tube, drawing);
                    let id = machine.id;
                    if remember(&mut self.library, id, |held| {
                        held.display = tube;
                        Some(())
                    }) {
                        self.save();
                    }
                    None
                }
                (Command::FullScreen, _) => {
                    let state = if machine.fullscreen {
                        WindowSizeState::Restored
                    } else {
                        WindowSizeState::Fullscreen
                    };
                    self.client
                        .set_size_state(window_id, state)
                        .err()
                        .map(|err| format!("{err}"))
                }
                (Command::Screenshot, _) => self.ask_screenshot(n),
                (_, None) => Some(String::from("the machine has not started")),
                (Command::Pause, Some(run)) => {
                    let asked = if machine.paused {
                        run.host.run()
                    } else {
                        run.host.pause()
                    };
                    asked.map_or_else(
                        |why| Some(format!("{why:?}")),
                        |()| {
                            machine.paused = !machine.paused;
                            let mut damage = Region::new();
                            machine.window.bar.set_paused(machine.paused, &mut damage);
                            machine.window.paint_bar(&damage, looks);
                            None
                        },
                    )
                }
                (Command::Reset, Some(run)) => run.host.reset().err().map(|why| format!("{why:?}")),
                (Command::CtrlAltDelete, Some(run)) => {
                    ctrl_alt_delete(&mut run.host);
                    None
                }
                (Command::CopyText, Some(run)) => match run.host.read_text() {
                    Ok(()) | Err(Refused::Busy) => None,
                    Err(why) => Some(format!("{why:?}")),
                },
                (Command::Paste, Some(_)) => match self.client.ask_clipboard(window_id) {
                    Ok(()) => {
                        machine.clipboard.pasting = true;
                        None
                    }
                    Err(err) => Some(format!("the clipboard could not be asked for ({err})")),
                },
                (Command::StopPasting, Some(run)) => {
                    machine.clipboard.pasting = false;
                    run.host.stop_typing();
                    None
                }
                (Command::Speed(rate), Some(run)) => match run.host.speed(rate) {
                    Ok(()) => {
                        machine.speed = rate;
                        None
                    }
                    Err(why) => Some(format!("{why:?}")),
                },
                (Command::ShareFolder, Some(_)) => machine.pick_share(&mut self.client),
            };
            if shut {
                self.stop(n, None);
            } else if command == Command::Debugger {
                self.open_debugger(n);
            } else if let Some(why) = refused {
                self.refuse("The machine did not take that", &why);
            } else if command == Command::Reset {
                // The host has let go of the stop, and of any look at it.
                let looks = (self.desktop.scale(), self.themes.active());
                if let Some(win) = self.machines[n].debugger.as_deref_mut() {
                    let mut damage = Region::new();
                    win.debugger.runs(&mut damage);
                    paint_debugger(win, &damage, looks);
                }
            }
        }

        /// Open machine `n`'s debugger, its probe put on the board; bring it
        /// forward where it is open.
        fn open_debugger(&mut self, n: usize) {
            let machine = &mut self.machines[n];
            if let Some(win) = &machine.debugger {
                let _ = self.client.activate_window(win.pane.id());
                return;
            }
            let Some(run) = machine
                .run
                .as_mut()
                .filter(|_| machine.life == Life::Running)
            else {
                self.refuse(
                    "The debugger could not be opened",
                    "the machine has not started",
                );
                return;
            };
            let (width, height) = self.desktop.window_size(DEBUGGER_SIZE.0, DEBUGGER_SIZE.1);
            let least = self.desktop.window_size(DEBUGGER_LEAST.0, DEBUGGER_LEAST.1);
            let mode = app::mode_for(width, height);
            let Some(surface) = Surface::new(mode.width_px, mode.height_px) else {
                self.refuse(
                    "The debugger could not be opened",
                    "there is not enough memory",
                );
                return;
            };
            let sizing = WindowSizing::Resizable {
                min_width_px: least.0,
                min_height_px: least.1,
                max_width_px: 0,
                max_height_px: 0,
            };
            let title = format!("{} \u{2014} {}", machine.name, debugging::TITLE);
            let pane = match WindowPane::open(
                &mut self.client,
                self.event_endpoint,
                &mode,
                &title,
                sizing,
            ) {
                Ok(pane) => pane,
                Err(err) => {
                    self.refuse("The debugger could not be opened", &format!("{err}"));
                    return;
                }
            };
            if let Err(why) = run.host.attach_debugger() {
                let _ = pane.close(&mut self.client);
                self.refuse("The debugger could not be opened", &format!("{why:?}"));
                return;
            }
            let fitted = self
                .library
                .machine(machine.id)
                .is_some_and(|held| held.config.coprocessor);
            let mut win = Box::new(DebugWindow {
                pane,
                surface,
                owed: Owed::new(),
                debugger: Debugger::new(fitted, (mode.width_px, mode.height_px)),
                adding: None,
            });
            let looks = (self.desktop.scale(), self.themes.active());
            let damage = whole(&win.pane);
            paint_debugger(&mut win, &damage, looks);
            machine.debugger = Some(win);
        }

        /// Close machine `n`'s debugger, its probe taken off and its machine
        /// going on from any stop.
        fn close_debugger(&mut self, n: usize) {
            let machine = &mut self.machines[n];
            let Some(win) = machine.debugger.take() else {
                return;
            };
            if let Some(adding) = win.adding {
                let _ = adding.pane.close(&mut self.client);
            }
            let _ = win.pane.close(&mut self.client);
            if let Some(run) = machine.run.as_mut() {
                let _ = run.host.detach_debugger();
            }
        }

        /// An event of machine `n`'s debugger window.
        fn debug_event(&mut self, n: usize, event: &WindowEvent) {
            if matches!(event, WindowEvent::CloseRequested { .. }) {
                self.close_debugger(n);
                return;
            }
            let Self {
                machines,
                client,
                desktop,
                themes,
                ..
            } = self;
            let looks = (desktop.scale(), themes.active());
            let machine = &mut machines[n];
            let Some(win) = machine.debugger.as_deref_mut() else {
                return;
            };
            let mut damage = Region::new();
            let mut ask = None;
            match *event {
                WindowEvent::Resized {
                    width_px,
                    height_px,
                    ..
                } => {
                    let mode = app::mode_for(width_px, height_px);
                    if win.pane.resize_with(client, &mode, &mut win.surface) == Fitted::Adopted {
                        ask = win.debugger.resize((mode.width_px, mode.height_px), looks);
                        damage = whole(&win.pane);
                    }
                }
                WindowEvent::ContentReleased { .. } => {
                    win.pane.release_frames();
                    damage = whole(&win.pane);
                }
                WindowEvent::RedrawRequested { .. } => win.owed.owe_whole(),
                _ => {
                    let at_ns = event_time(event);
                    let debugger = &mut win.debugger;
                    inputs(event, |input| {
                        ask = ask.or(debugger.on_input(input, at_ns, looks, &mut damage));
                    });
                }
            }
            paint_debugger(win, &damage, looks);
            let Some(ask) = ask else {
                return;
            };
            let opens = match machine.run.as_mut() {
                Some(run) => relay(&mut run.host, &mut win.debugger, ask),
                None => false,
            };
            if opens {
                self.open_adding(n);
            }
        }

        /// Open the dialog machine `n`'s debugger adds a breakpoint through.
        fn open_adding(&mut self, n: usize) {
            let (scale, theme) = (self.desktop.scale(), self.themes.active());
            let Some(win) = self.machines[n].debugger.as_deref_mut() else {
                return;
            };
            if win.adding.is_some() {
                return;
            }
            let adding = Adding::new(win.debugger.registers().unwrap_or_default());
            let (width, height) = adding.extent(scale, theme);
            let mode = app::mode_for(width, height);
            let Some(surface) = Surface::new(mode.width_px, mode.height_px) else {
                app::report(APP_NAME, "there is not enough memory to draw the dialog");
                return;
            };
            match WindowPane::open_dialog(
                &mut self.client,
                win.pane.id(),
                self.event_endpoint,
                &mode,
                DialogModality::OwnerModal,
                debugging::ADDING_TITLE,
            ) {
                Ok(pane) => {
                    let mut dialog = AddingWindow {
                        pane,
                        surface,
                        owed: Owed::new(),
                        adding,
                    };
                    dialog.adding.render(&mut dialog.surface, scale, theme);
                    dialog.owed.owe_whole();
                    win.adding = Some(dialog);
                }
                Err(err) => app::report(
                    APP_NAME,
                    format_args!("the breakpoint dialog could not be opened ({err})"),
                ),
            }
        }

        /// An event of machine `n`'s breakpoint dialog.
        fn adding_event(&mut self, n: usize, event: &WindowEvent) {
            let Self {
                machines,
                client,
                desktop,
                themes,
                ..
            } = self;
            let looks = (desktop.scale(), themes.active());
            let machine = &mut machines[n];
            let Some(win) = machine.debugger.as_deref_mut() else {
                return;
            };
            let Some(dialog) = win.adding.as_mut() else {
                return;
            };
            let mut answer = None;
            let mut damage = Region::new();
            match *event {
                WindowEvent::CloseRequested { .. } => answer = Some(Added::Cancel),
                WindowEvent::ContentReleased { .. } => {
                    dialog.pane.release_frames();
                    damage = whole(&dialog.pane);
                }
                WindowEvent::RedrawRequested { .. } => dialog.owed.owe_whole(),
                _ => {
                    let at_ns = event_time(event);
                    let adding = &mut dialog.adding;
                    inputs(event, |input| {
                        answer =
                            answer
                                .take()
                                .or(adding.on_input(input, at_ns, looks, &mut damage));
                    });
                }
            }
            if !damage.is_empty() {
                let adding = &dialog.adding;
                repaint(
                    &dialog.pane,
                    &mut dialog.surface,
                    &mut dialog.owed,
                    &damage,
                    |surface| {
                        adding.render(surface, looks.0, looks.1);
                    },
                );
            }
            let Some(answer) = answer else {
                return;
            };
            if let Some(dialog) = win.adding.take() {
                let _ = dialog.pane.close(client);
            }
            let Added::Add(entry) = answer else {
                return;
            };
            let mut damage = Region::new();
            let ask = win.debugger.add(entry, looks, &mut damage);
            paint_debugger(win, &damage, looks);
            if let (Some(ask), Some(run)) = (ask, machine.run.as_mut()) {
                relay(&mut run.host, &mut win.debugger, ask);
            }
        }

        /// Ask where machine `n`'s screen, as it stands now, is kept: why
        /// not, where it cannot be.
        fn ask_screenshot(&mut self, n: usize) -> Option<String> {
            let machine = &mut self.machines[n];
            if machine.window.picture.screen().size().0 == 0 {
                return Some(String::from("nothing is on its screen yet"));
            }
            let Some(screen) = LastScreen::of(machine.window.picture.screen()) else {
                return Some(String::from("there is not enough memory"));
            };
            let named = format!("{} screen.png", machine.name);
            let suggested = DocumentName::new(&named).or_else(|_| DocumentName::new("screen.png"));
            let mut formats = SaveFormats::ANY;
            let (Ok(suggested), Ok(())) = (
                suggested,
                formats.push(save_format(MediaType::ImagePng), true),
            ) else {
                return Some(String::from("its name could not be offered"));
            };
            let window_id = machine.window.pane.id();
            match self
                .client
                .pick_file(window_id, PickPurpose::Save { suggested, formats })
            {
                Ok(()) => {
                    machine.picking = Some(Pick::Screenshot(screen));
                    None
                }
                Err(err) => Some(format!("the picker could not be asked for ({err})")),
            }
        }

        /// Hand the file `grant` delegates, called `name`, to the disk worker
        /// for machine `n`'s diskette drive `unit`.
        fn take_file(&mut self, n: usize, unit: u8, (grant, writable): (u64, bool), name: String) {
            let machine = &mut self.machines[n];
            if machine.run.is_none() {
                drop(File::from_delegation(grant));
                let headline = format!("\u{201c}{}\u{201d} has not started", machine.name);
                self.refuse(&headline, "A disk goes in once the machine is running.");
                return;
            }
            let Some(coming) = machine.coming.get_mut(usize::from(unit)) else {
                drop(File::from_delegation(grant));
                return;
            };
            if coming.is_some() {
                drop(File::from_delegation(grant));
                self.refuse(
                    "The disk could not be inserted",
                    "Another disk is still going into that drive.",
                );
                return;
            }
            *coming = Some(Diskette {
                name,
                read_only: !writable,
                medium: 0,
            });
            let id = machine.id;
            let job = DiskJob::Take {
                id,
                unit,
                grant,
                writable,
            };
            if self.disks.submit(job).is_err() {
                self.refuse(
                    "The disk could not be inserted",
                    "too much is waiting on the disks",
                );
            }
        }

        /// The disk worker took a file for machine `id`'s diskette drive
        /// `unit`: put it in.
        fn taken(&mut self, id: u32, unit: u8, taken: Result<(FileMedium, Form), String>) {
            // A take no disk is still coming for was made of a machine that
            // has stopped since.
            let Some(n) = self.machine_index(id).filter(|&n| {
                self.machines[n]
                    .coming
                    .get(usize::from(unit))
                    .is_some_and(Option::is_some)
            }) else {
                return;
            };
            let (file, form) = match taken {
                Ok(taken) => taken,
                Err(why) => {
                    if let Some(coming) = self.machines[n].coming.get_mut(usize::from(unit)) {
                        *coming = None;
                    }
                    self.refuse("The disk could not be inserted", &why);
                    return;
                }
            };
            let machine = &mut self.machines[n];
            let Some(run) = &mut machine.run else {
                return;
            };
            let medium = match run.host.insert(unit, form) {
                Ok(medium) => medium,
                Err(why) => {
                    if let Some(coming) = machine.coming.get_mut(usize::from(unit)) {
                        *coming = None;
                    }
                    let why = format!("the machine refused it ({why:?})");
                    self.refuse("The disk could not be inserted", &why);
                    return;
                }
            };
            if let Some(Some(coming)) = machine.coming.get_mut(usize::from(unit)) {
                coming.medium = medium;
            }
            let job = DiskJob::Attach {
                id,
                medium,
                file: Some(file),
            };
            if self.disks.submit(job).is_err() {
                self.refuse(
                    "The disk could not be inserted",
                    "too much is waiting on the disks",
                );
            }
        }

        /// Serve the members of the program's own the park found ready.
        fn serve_members(&mut self, fired: u64) {
            for machine in &mut self.machines {
                let [read, write, sounding] = machine_tokens(machine.slot);
                if let Some(run) = &mut machine.run {
                    if fired & member_bit(read) != 0 {
                        run.host.on_readable();
                    }
                    if fired & member_bit(write) != 0 {
                        run.host.on_writable();
                    }
                }
                if fired & member_bit(sounding) != 0 {
                    if let Some((opened, waiting)) = &mut machine.sound {
                        while let Ok(Some(_)) = opened.stream.take_notify(&mut opened.audio) {}
                        let _ = waiting.feed(&mut Ring(&mut opened.stream));
                    }
                }
            }
        }

        /// Act on everything every machine did.
        fn pump(&mut self) {
            let mut n = 0;
            while n < self.machines.len() {
                match self.pump_one(n) {
                    Some(Leave::Ended(why)) => self.stop(n, Some(why)),
                    Some(Leave::Suspended(kept)) => self.suspended_whole(n, kept),
                    Some(Leave::SetAside(screen)) => {
                        let id = self.machines[n].id;
                        let asked = self.ask(Job::Keep {
                            id,
                            screen,
                            suspended: None,
                            picture: None,
                            why: Keeping::SetAside,
                        });
                        if asked {
                            n += 1;
                        } else {
                            self.stop(n, Some(String::from("the store is too busy")));
                        }
                    }
                    None => n += 1,
                }
            }
            for (headline, why) in core::mem::take(&mut self.notes) {
                self.refuse(&headline, &why);
            }
        }

        /// Act on everything machine `n` did: what is left to do of its
        /// leaving or its resuming, if anything.
        fn pump_one(&mut self, n: usize) -> Option<Leave> {
            let Self {
                machines,
                client,
                disks,
                blocks,
                library,
                set,
                desktop,
                themes,
                notes,
                pool,
                ..
            } = self;
            let (set, looks) = (*set, (desktop.scale(), themes.active()));
            let drawing = drawing(pool, desktop);
            let mut damage = Region::new();
            let machine = &mut machines[n];
            let (id, name, ticket) = (machine.id, machine.name.as_str(), machine.ticket);
            let run = machine.run.as_mut()?;
            let mut settled = Settled::default();
            let mut leave = None;
            let target = blocks.as_ref().map(|blocks| blocks.target);
            while let Some(event) = run.host.event() {
                match owned(id, &event, target) {
                    Some(Owned::Moment(moment)) => {
                        let lived = Lived {
                            life: &mut machine.life,
                            ran: &mut machine.ran,
                            resumed: &mut machine.resumed,
                            screen: machine.window.picture.screen(),
                        };
                        leave = lived.take(moment, &mut run.host).or(leave);
                        continue;
                    }
                    Some(Owned::Handed(handed)) => {
                        let workers = (&**disks, blocks.as_ref());
                        hand_on(handed, workers, (ticket, run.worker), &mut run.host);
                        continue;
                    }
                    None => {}
                }
                match event {
                    HostEvent::Frame {
                        adapter: 0,
                        width,
                        height,
                        pixels,
                    } => {
                        if machine.window.picture.take(width, height, pixels) {
                            machine.window.draw(drawing);
                        }
                    }
                    HostEvent::Audio(pcm) => {
                        if let Some(fill) = play(&mut machine.sound, pcm, (set, machine.slot)) {
                            run.host.fill(fill);
                        }
                    }
                    HostEvent::Pointing { follows } => {
                        let asks = machine.window.input.pointing(&mut run.host, follows);
                        machine.window.ask(client, asks, name);
                    }
                    HostEvent::Text(outcome) => {
                        if let Err(why) = copy_text(client, machine.window.pane.id(), outcome) {
                            notes.push((String::from("Nothing was copied"), why));
                        }
                    }
                    HostEvent::TextShown(shown) => machine.clipboard.copyable = shown,
                    heard @ (HostEvent::Stopped { .. } | HostEvent::Seen(_)) => {
                        if let Some(win) = machine.debugger.as_deref_mut() {
                            if let Some(ask) = debugger_heard(win, heard, looks) {
                                relay(&mut run.host, &mut win.debugger, ask);
                            }
                        }
                    }
                    HostEvent::Shared { slot, drive } => {
                        let held = (&mut machine.shares, &mut machine.window.bar);
                        shared(held, (slot, drive), looks, &mut damage);
                    }
                    HostEvent::Interpreting => Self::interpreted(name, ticket, blocks.as_ref()),
                    other => {
                        let parts = Parts {
                            id,
                            diskettes: (&mut machine.coming, &mut machine.held),
                            bar: &mut machine.window.bar,
                            disks,
                            library,
                            notes,
                            damage: &mut damage,
                        };
                        settled.fold(remember_event(other, parts));
                    }
                }
            }
            if !damage.is_empty() {
                machine.window.paint_bar(&damage, looks);
            }
            if settled.changed {
                self.save();
            }
            settled.ended.map(Leave::Ended).or(leave)
        }

        /// Machine `name`, started as `ticket`, ran translated code that
        /// misbehaved: said, and its compiler let go, as it has nothing more to
        /// do.
        fn interpreted(name: &str, ticket: u64, blocks: Option<&Blocks>) {
            app::report(
                APP_NAME,
                format_args!(
                    "\u{201c}{name}\u{201d} is interpreted from now on: its translated code misbehaved"
                ),
            );
            if let Some(blocks) = blocks {
                let _ = blocks.relay.submit(CompileJob::Forget { machine: ticket });
            }
        }

        /// Register the machines' pipes as they want them, and present what
        /// every window owes.
        fn settle(&mut self) -> Result<(), Errno> {
            for machine in &mut self.machines {
                if let Some(run) = &mut machine.run {
                    let _ = run.members.sync(
                        run.host.descriptors(),
                        run.host.wants_read(),
                        run.host.wants_write(),
                    );
                }
            }
            while let Some(answer) = self.client.take_answer() {
                match answer {
                    Landed::FrameReturned { window_id } => self.frame_returned(window_id)?,
                    Landed::PickedName { window_id, name } => self.picked_name(window_id, name),
                    Landed::OfferedFile(offer) => {
                        let disk = offer
                            .media
                            .as_deref()
                            .ok()
                            .and_then(MediaType::from_media_str)
                            .is_some_and(|media| {
                                matches!(
                                    media,
                                    MediaType::RawFloppyDiskImage
                                        | MediaType::RawDiskImage
                                        | MediaType::ImageDisk
                                )
                            });
                        let _ = self
                            .client
                            .answer_file_offer(offer.window_id, offer.serial, disk);
                    }
                    Landed::OpenTarget(target) => self.open_target(target),
                    Landed::Clipboard {
                        window_id,
                        contents,
                    } => self.pasted(window_id, contents),
                    _ => {}
                }
            }
            self.follow_menus();
            let client = &mut self.client;
            if let Some(win) = &mut self.shelf {
                win.present(client)?;
            }
            if let Some(win) = &mut self.sheet {
                win.present(client)?;
            }
            for win in self
                .machines
                .iter_mut()
                .filter_map(|machine| machine.debugger.as_deref_mut())
            {
                present(&mut win.pane, &win.surface, &mut win.owed, client)?;
                if let Some(dialog) = &mut win.adding {
                    present(&mut dialog.pane, &dialog.surface, &mut dialog.owed, client)?;
                }
            }
            let failed = self
                .machines
                .iter_mut()
                .enumerate()
                .find_map(|(n, machine)| {
                    let window = &mut machine.window;
                    present(&mut window.pane, &window.surface, &mut window.owed, client)
                        .err()
                        .map(|err| (n, err))
                });
            if let Some((n, err)) = failed {
                self.stop(n, Some(format!("its window could not be drawn ({err})")));
            }
            Ok(())
        }

        /// What the clipboard held for machine window `window_id`'s paste,
        /// typed on its keyboard as far as it can be.
        fn pasted(
            &mut self,
            window_id: u64,
            contents: Result<Option<(ClipboardKind, Vec<u8>)>, Errno>,
        ) {
            let Some(machine) = self
                .machines
                .iter_mut()
                .find(|machine| machine.window.pane.id() == window_id)
            else {
                return;
            };
            // Stopped before it came, or never asked.
            if !core::mem::take(&mut machine.clipboard.pasting) {
                return;
            }
            let live = machine.life == Life::Running;
            let typed = match contents {
                Ok(Some((ClipboardKind::Text, bytes))) => match text::typing(&bytes) {
                    Ok(Some(typing)) if !typing.codes.is_empty() => Ok(typing),
                    Ok(_) => Err(String::from(
                        "the clipboard holds nothing this machine can type",
                    )),
                    Err(_) => Err(String::from("there was no memory for its text")),
                },
                Ok(Some(_)) => Err(String::from("the clipboard holds no text")),
                Ok(None) => Err(String::from("the clipboard is empty")),
                Err(err) => Err(format!("the clipboard could not be read ({err})")),
            };
            let outcome = typed.and_then(|typing| {
                let run = machine
                    .run
                    .as_mut()
                    .filter(|_| live)
                    .ok_or_else(|| String::from("the machine has not started"))?;
                let shortfall = typing.shortfall();
                run.host
                    .type_text(typing.codes)
                    .map_err(|why| format!("{why:?}"))?;
                Ok(shortfall)
            });
            match outcome {
                Ok(Some((headline, detail))) => self.warn_over(Some(window_id), headline, &detail),
                Ok(None) => {}
                Err(why) => self.refuse_over(Some(window_id), "Nothing was pasted", &why),
            }
        }

        /// The name of the file machine window `window_id` picked.
        fn picked_name(&mut self, window_id: u64, name: Result<String, Errno>) {
            let looks = (self.desktop.scale(), self.themes.active());
            let Some(n) = self
                .machines
                .iter()
                .position(|machine| machine.window.pane.id() == window_id)
            else {
                return;
            };
            if let Some(sharing) = self.machines[n].sharing.take() {
                self.share(n, sharing, name.as_deref().unwrap_or_default());
                return;
            }
            let machine = &mut self.machines[n];
            let (Some(unit), Ok(name)) = (machine.naming.take(), name) else {
                return;
            };
            let index = usize::from(unit);
            if let Some(Some(coming)) = machine.coming.get_mut(index) {
                coming.name = name;
            } else if let Some(Some(held)) = machine.held.get_mut(index) {
                held.name = name;
                let shown = (shown_name(&held.name), held.read_only);
                let mut damage = Region::new();
                machine.window.bar.set_held(unit, Some(shown), &mut damage);
                machine.window.paint_bar(&damage, looks);
            }
        }

        /// A file the desktop handed over to be opened: a disk image dropped
        /// on a machine window goes into the diskette drive under it,
        /// read-only.
        fn open_target(&mut self, target: Result<Option<tairix_window::Target>, Errno>) {
            match target {
                Ok(Some(tairix_window::Target::Placed {
                    window_id,
                    at,
                    name,
                    grant,
                })) => {
                    let found = self
                        .machines
                        .iter()
                        .position(|machine| machine.window.pane.id() == window_id);
                    let point = pointer_point(at.0, at.1);
                    let unit = found.and_then(|n| self.machines[n].window.bar.diskette_at(point));
                    match found.zip(unit) {
                        Some((n, unit)) => self.take_file(n, unit, (grant, false), name),
                        None => drop(File::from_delegation(grant)),
                    }
                }
                Ok(Some(tairix_window::Target::Document { name, grant, .. })) => {
                    drop(File::from_delegation(grant));
                    app::report(
                        APP_NAME,
                        format_args!(
                            "{name} was handed over to open; a disk is dropped on a machine"
                        ),
                    );
                }
                Ok(Some(_) | None) => {}
                Err(err) => app::report(
                    APP_NAME,
                    format_args!("what was handed over could not be read ({err})"),
                ),
            }
        }

        /// Present what window `window_id` drew while both its frames were
        /// the desktop's. A machine window that cannot be drawn stops its
        /// machine; the library's or a dialog's ends the program.
        fn frame_returned(&mut self, window_id: u64) -> Result<(), Errno> {
            let client = &mut self.client;
            if let Some(win) = self.shelf.as_mut().filter(|win| win.id() == window_id) {
                return win.present_deferred(client);
            }
            if let Some(win) = self.sheet.as_mut().filter(|win| win.id() == window_id) {
                return win.present_deferred(client);
            }
            let failed = self
                .machines
                .iter()
                .position(|machine| machine.window.pane.id() == window_id)
                .and_then(|n| {
                    let window = &mut self.machines[n].window;
                    window
                        .pane
                        .present_deferred(client, &window.surface)
                        .err()
                        .map(|err| (n, err))
                });
            if let Some((n, err)) = failed {
                self.stop(n, Some(format!("its window could not be drawn ({err})")));
            }
            Ok(())
        }

        /// The machine's memory-pressure band moved.
        fn pressure(&mut self) {
            let band = tairix_rt::pressure::gauge().band();
            for machine in &mut self.machines {
                if let Some(run) = &mut machine.run {
                    run.host.pressure(band);
                }
            }
            tairix_font::trim_glyph_cache();
        }

        /// The desktop's look moved: draw everything in it again.
        fn desktop_moved(&mut self) {
            match app::adopt_desktop(&mut self.desktop, &mut self.themes) {
                Ok(true) => {}
                Ok(false) => return,
                Err(err) => {
                    app::report(
                        APP_NAME,
                        format_args!("the desktop's new look could not be read ({err})"),
                    );
                    return;
                }
            }
            let size = Shelf::picture_size(self.desktop.scale(), self.themes.active());
            if self.pictures.resize(size) {
                if let Some(picture) = size {
                    let ids = self
                        .library
                        .machines()
                        .iter()
                        .map(|machine| machine.id)
                        .collect();
                    self.ask(Job::Pictures { ids, picture });
                }
            }
            let (scale, theme) = (self.desktop.scale(), self.themes.active());
            if let Some(win) = &mut self.shelf {
                win.relook((scale, theme), &self.pictures);
            }
            if let Some(win) = &mut self.sheet {
                win.relook(&mut self.client, (scale, theme));
            }
            let drawing = drawing(&self.pool, &self.desktop);
            for machine in &mut self.machines {
                machine.window.redraw((scale, theme), drawing);
                if let Some(win) = machine.debugger.as_deref_mut() {
                    let damage = whole(&win.pane);
                    paint_debugger(win, &damage, (scale, theme));
                }
            }
        }

        /// Act on an event for the whole application.
        fn app_event(&mut self, event: &WindowEvent) {
            match *event {
                // Leaving, it opens nothing new and is not asked to leave again.
                WindowEvent::AppBarDefault | WindowEvent::AppBarMenu { .. }
                    if self.leaving.is_some() => {}
                WindowEvent::AppBarDefault | WindowEvent::AppBarMenu { .. } => {
                    match library_window::bar_ask(event) {
                        Some(BarAsk::Quit) => self.leave(),
                        Some(BarAsk::Library) => self.open_shelf(),
                        None => {}
                    }
                }
                WindowEvent::OpenRequested => {
                    if let Err(err) = self.client.ask_open_targets() {
                        app::report(
                            APP_NAME,
                            format_args!("what was handed over could not be asked for ({err})"),
                        );
                    }
                }
                WindowEvent::AlertClosed { alert_id, outcome } => {
                    if let Some((asked, id)) = self.asking {
                        if asked == alert_id {
                            self.asking = None;
                            if outcome == (AlertOutcome::Chosen { index: 0 }) {
                                self.delete(id);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        /// Route one window event to the window it names.
        fn handle(&mut self, event: &WindowEvent) {
            let Some(window_id) = event.window_id() else {
                self.app_event(event);
                return;
            };
            if self.shelf.as_ref().is_some_and(|win| win.id() == window_id) {
                self.shelf_event(event);
            } else if self.sheet.as_ref().is_some_and(|win| win.id() == window_id) {
                self.sheet_event(event);
            } else if let Some(n) = self
                .machines
                .iter()
                .position(|machine| machine.window.pane.id() == window_id)
            {
                self.machine_event(n, event);
            } else if let Some((n, dialog)) =
                self.machines.iter().enumerate().find_map(|(n, machine)| {
                    let win = machine.debugger.as_deref()?;
                    if win.pane.id() == window_id {
                        Some((n, false))
                    } else {
                        win.adding
                            .as_ref()
                            .filter(|adding| adding.pane.id() == window_id)
                            .map(|_| (n, true))
                    }
                })
            {
                if dialog {
                    self.adding_event(n, event);
                } else {
                    self.debug_event(n, event);
                }
            } else {
                app::release_carried(event);
            }
        }

        /// When the next step is owed: the library's held scrollbar press,
        /// a machine's tube, a suspend's deadline, or the end of leaving.
        fn due(&mut self) -> Option<u64> {
            let now = tairix_rt::clock_get();
            let scroll = self.shelf.as_mut().and_then(|win| win.deadline(now));
            let tubes = self
                .machines
                .iter()
                .filter_map(|machine| machine.window.picture.due());
            let suspends = self.machines.iter().filter_map(|machine| machine.answer_by);
            tubes
                .chain(scroll)
                .chain(self.leaving)
                .chain(suspends)
                .min()
        }

        /// Draw every machine's tube whose animation step is due.
        fn step_tubes(&mut self) {
            let drawing = drawing(&self.pool, &self.desktop);
            for machine in &mut self.machines {
                machine.window.step_tube(drawing);
            }
        }

        /// Close every window, turning off what is still running, and give
        /// the disks and then the store until a deadline to take what they
        /// were last asked to keep.
        fn shutdown(&mut self) {
            self.close_sheet();
            if let Some(win) = self.shelf.take() {
                win.close(&mut self.client);
            }
            while !self.machines.is_empty() {
                self.stop(0, None);
            }
            if self.writes.failed() {
                self.save();
            }
            let deadline = tairix_rt::clock_get().saturating_add(DRAIN_NS);
            while self.disks.outstanding() {
                match self.disks.wait_until(deadline) {
                    Some(done) => self.served(done),
                    None => break,
                }
            }
            while self.store.outstanding() {
                match self.store.wait_until(deadline) {
                    Some(Done::Recorded(result)) => self.recorded(result),
                    Some(Done::Started {
                        launched: Ok(launched),
                        ..
                    }) => {
                        if let Some(sound) = launched.sound {
                            sound.close();
                        }
                    }
                    Some(Done::Kept {
                        id, why, result, ..
                    }) => self.state_kept(id, why, result),
                    Some(_) => {}
                    None => break,
                }
            }
            if !self.keeping.is_empty() {
                app::report(
                    APP_NAME,
                    format_args!(
                        "{} suspended machines could not be kept in time, and start afresh",
                        self.keeping.len()
                    ),
                );
            }
        }
    }

    /// Play `pcm` on a machine's sound: the frames the machine should render
    /// ahead next. A stream that fails is closed, and the machine plays on
    /// silently; the machine in `slot` had its notifications on `set`.
    fn play(
        sound: &mut Option<(Opened, Sound)>,
        pcm: &[u8],
        (set, slot): (u64, usize),
    ) -> Option<u32> {
        let (opened, waiting) = sound.as_mut()?;
        let mut ring = Ring(&mut opened.stream);
        if waiting.take(pcm, &mut ring).is_ok() {
            return Some(waiting.fill(&mut ring));
        }
        app::report(APP_NAME, "the sound stream failed; the machine is silent");
        if let Some((opened, _)) = sound.take() {
            let [_, _, sounding] = machine_tokens(slot);
            unwatch_sound(set, &opened, sounding);
            opened.close();
        }
        None
    }

    /// Make `change` to machine `id` of `library`: whether it was made.
    fn remember(
        library: &mut Library<Machine>,
        id: u32,
        change: impl FnOnce(&mut Machine) -> Option<()>,
    ) -> bool {
        let Some(mut held) = library.machine(id).cloned() else {
            return false;
        };
        change(&mut held).is_some() && library.replace(held).is_ok()
    }

    /// Take a stream's notifications off wait-set `set`.
    fn unwatch_sound(set: u64, opened: &Opened, token: u64) {
        let _ = tairix_rt::waitset_ctl(
            set,
            WaitSetOp::Del,
            WaitSourceKind::Port,
            opened.stream.notify_port(),
            token,
        );
    }

    /// Bring up the desktop, the workers and the library window, and read the
    /// library.
    fn bring_up() -> Result<(App, EventMailbox), i32> {
        let mut client = WindowClient::new(app::RtWindowTransport);
        let (desktop, themes) = app::bring_up_desktop(&mut client)
            .map_err(|err| app::fail(APP_NAME, err.code(), err))?;
        let Some(server) = client.session() else {
            return Err(app::fail(
                APP_NAME,
                app::EXIT_NO_WINDOW,
                "the desktop did not identify itself",
            ));
        };
        let binding =
            app::bind_event_mailbox().map_err(|err| app::fail(APP_NAME, err.code(), err))?;
        let (event_endpoint, set) = (binding.endpoint(), binding.set());
        if let Err(why) = library_window::declare_bar(&mut client, event_endpoint) {
            app::report(APP_NAME, why);
        }
        let no_room = |_| {
            app::fail(
                APP_NAME,
                app::EXIT_NO_WINDOW,
                "there is no room for the application's workers",
            )
        };
        let store = Worker::queued(serve_store, (), WorkerWake::create(), STORE_JOBS)
            .map(Arc::new)
            .map_err(no_room)?;
        let disks = Worker::queued(
            serve_disk,
            Disks::default(),
            WorkerWake::create(),
            DISK_JOBS,
        )
        .map(Arc::new)
        .map_err(no_room)?;
        app::start_worker(APP_NAME, &store, set, STORE_TOKEN, "store");
        app::start_worker(APP_NAME, &disks, set, DISK_TOKEN, "disk");
        let blocks = start_blocks(set);
        let picture = Shelf::picture_size(desktop.scale(), themes.active());
        let mut app = App {
            client,
            event_endpoint,
            set,
            desktop,
            themes,
            store,
            disks,
            blocks,
            retiring: Vec::new(),
            library: Library::new(),
            shelving: Shelving::Reading,
            pending: Vec::new(),
            writes: Writes::default(),
            shelf: None,
            sheet: None,
            card_menu: None,
            asking: None,
            machines: Vec::new(),
            pool: LazyPool::new(
                APP_NAME,
                tairix_procinfo::online_cpus(&tairix_procinfo::IpcTransport),
                "drawing the picture tube",
            ),
            notes: Vec::new(),
            tickets: 0,
            pictures: {
                let mut pictures = Pictures::default();
                pictures.resize(picture);
                pictures
            },
            suspended: Vec::new(),
            keeping: Suspends::new(),
            leaving: None,
            quit: false,
        };
        app.open_shelf();
        if app.shelf.is_none() {
            return Err(app::fail(
                APP_NAME,
                app::EXIT_NO_WINDOW,
                "the library window could not be opened",
            ));
        }
        app.ask(Job::Load { picture });
        Ok((app, EventMailbox::new(event_endpoint, server)))
    }

    fn run() -> i32 {
        let (mut app, mailbox) = match bring_up() {
            Ok(up) => up,
            Err(code) => return code,
        };
        let _guards = (
            WorkerGuard::new(&app.store),
            WorkerGuard::new(&app.disks),
            app.blocks
                .as_ref()
                .map(|blocks| WorkerGuard::new(&blocks.relay)),
        );
        let woke = Cell::new(Woke::default());
        let due = Cell::new(None);
        let (store, disks) = (Arc::clone(&app.store), Arc::clone(&app.disks));
        let relay = app.blocks.as_ref().map(|blocks| Arc::clone(&blocks.relay));
        let mut events = WindowEvents::new(RtEventSource {
            mailbox,
            set: app.set,
            wakes: [store.wake(), disks.wake()],
            compiles: relay.as_deref().map(Worker::wake),
            woke: &woke,
            due: &due,
        });
        let mut woken = None;
        let ended = loop {
            let fired = woke.replace(Woke::default());
            app.collect();
            app.serve_members(fired.members);
            if fired.pressure {
                app.pressure();
            }
            if fired.desktop {
                app.desktop_moved();
            }
            if fired.due {
                app.step_shelf();
                app.step_tubes();
                app.give_up_suspends();
                app.leave_if_due();
            }
            app.pump();
            match serve_events(&mut app, &mut events, &mut woken) {
                Ok(drained) => {
                    if app.quit {
                        break 0;
                    }
                    if let Err(err) = app.settle() {
                        break app::fail(
                            APP_NAME,
                            app::EXIT_CHANNEL_LOST,
                            format_args!("present refused ({err})"),
                        );
                    }
                    if !drained {
                        continue;
                    }
                }
                Err(code) => break code,
            }
            due.set(app.due());
            match events.wait(&mut app.client) {
                Ok(Some(event)) => woken = Some(event),
                Ok(None) => {}
                Err(EventError::Mailbox(err)) => break lost(err),
                Err(other) => app::report(APP_NAME, format_args!("{other:?}")),
            }
        };
        app.shutdown();
        ended
    }

    /// Hand the window events waiting to the windows they name, the one the
    /// last park ended on first: whether the mailbox was drained.
    fn serve_events(
        app: &mut App,
        events: &mut WindowEvents<RtEventSource<'_>>,
        woken: &mut Option<WindowEvent>,
    ) -> Result<bool, i32> {
        for _ in 0..EVENT_MAILBOX_CAPACITY {
            let delivered = match woken.take() {
                Some(event) => Ok(Some(event)),
                None => events.try_wait(&mut app.client),
            };
            match delivered {
                Ok(Some(event)) => {
                    app.handle(&event);
                    if app.quit {
                        return Ok(true);
                    }
                }
                Ok(None) => return Ok(true),
                Err(EventError::Mailbox(err)) => return Err(lost(err)),
                Err(other) => app::report(APP_NAME, format_args!("{other:?}")),
            }
        }
        Ok(false)
    }

    /// The exit for a desktop that went away.
    fn lost(err: Errno) -> i32 {
        app::fail(
            APP_NAME,
            app::EXIT_CHANNEL_LOST,
            format_args!("the desktop went away ({err})"),
        )
    }

    /// The executor the machine worker runs the blocks installed in it on.
    #[allow(unsafe_code)] // The executor's promise, kept as stated below.
    fn native() -> Box<dyn Execute> {
        // SAFETY: made only for the machine worker's role, whose code comes
        // over its session, whose other end is the front-end that started it;
        // the front-end alone installs code in it, and names only addresses
        // `code_install` answered for the blocks its compiler sandbox compiled
        // with this binary's translator, for this processor, and staged whole.
        Box::new(unsafe { Native::new(Box::new(roles::unmap_code)) })
    }

    fn main() -> i32 {
        if let Some(exit) = roles::served(|share| (share, native())) {
            return exit;
        }
        if tairix_rt::arg(1).is_some_and(|arg| matches!(arg, b"-h" | b"--help" | b"-?")) {
            return tairix_help::print_own_short_help(APP_NAME, None);
        }
        run()
    }

    tairix_rt::entry!(main);
}

#[cfg(not(freestanding))]
fn main() {}

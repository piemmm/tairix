//! The `Run` entry point of the `pcemucheck` fixture: the sandbox roles
//! `PCEmulator.app`'s own `Run` plays — a machine's worker, here with its
//! native executor counting the blocks it runs into its share's tail, and a
//! compiler — and the owner, which checks the reference run and then runs the
//! default machine until the program on its diskette has printed its sum,
//! installing the code its compiler sandbox stages into the worker.
//!
//! On the host it is an inert stub so the workspace tooling still covers the
//! file.

#![cfg_attr(all(freestanding, feature = "program"), no_std)]
#![cfg_attr(all(freestanding, feature = "program"), no_main)]
#![deny(missing_docs)]

#[cfg(all(freestanding, feature = "program"))]
mod program {
    extern crate alloc;

    use alloc::boxed::Box;
    use alloc::format;
    use alloc::rc::Rc;
    use alloc::string::String;
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use core::num::NonZeroUsize;

    use tairix_abi::{Errno, PAGE_SIZE, SPAWN_SELF};
    use tairix_codegen::Target;
    use tairix_hash::HashSeed;
    use tairix_pcemu::cpu::jit::cache::Key;
    use tairix_pcemu::cpu::jit::compiler::Compile;
    use tairix_pcemu::cpu::jit::dispatch::{Env, Execute};
    use tairix_pcemu::cpu::jit::frame::Frame;
    use tairix_pcemu::cpu::jit::native::Native;
    use tairix_pcemu::cpu::jit::page::PageMap;
    use tairix_pcemu::cpu::jit::Exit;
    use tairix_pcemu::digest::{reference_run, REFERENCE_DIGEST};
    use tairix_pcemu::dos::setup::{default_disk, DefaultDisk};
    use tairix_pcemu::front::{Host, HostEvent};
    use tairix_pcemu::link::layout::Layout;
    use tairix_pcemu::link::{FixedDisk, Form, InsertRefusal, Setup};
    use tairix_pcemu::machine::Template;
    use tairix_pcemu::roles::{self, HeldCompiler};
    use tairix_pcemu::storage::sparse::SparseDisk;
    use tairix_pcemu::storage::{BlockDevice, MediaError, SECTOR};
    use tairix_rt::io::{write_stderr_line, Stdout, Write};
    use tairix_rt::shm::SpawnPart;
    use tairix_rt::LogSink;
    use tairix_sandbox::rt::{worker_ceiling, RtSessionChannel, SessionMembers};
    use tairix_sandbox::session::{SessionTransport, Share, ShareWrite};
    use tairix_sandbox::WorkerMemory;
    use tairix_test_pcemu::{answered, at_prompt, pass_line, sum_line, TAIL, TYPED};

    /// The machine: the one a new library is given.
    const MACHINE: Template = Template::TurboXt;

    /// The default disk's volume serial number.
    const SERIAL: u32 = 0x5043_454D;

    /// The disk's name in the session.
    const MEDIUM: u16 = 1;

    /// The drive the diskette goes in: A:.
    const DRIVE: u8 = 0;

    /// The frames of sound the worker steers the owner's buffer to: what
    /// `PCEmulator.app` asks for, though nothing here plays them.
    const AUDIO_TARGET: u32 = 2_048;

    /// How long the machine has, by the host's clock, to boot and print the
    /// program's sum: many times what that takes on the slowest emulated host.
    const BUDGET_NS: u64 = 400_000_000_000;

    /// How often the owner reads the screen while it waits.
    const LOOK_NS: u64 = 250_000_000;

    /// Wait-set tokens: the session's read end, its write end, and the
    /// memory-pressure notice.
    const READABLE: u64 = 1;
    const WRITABLE: u64 = 2;
    const PRESSURE: u64 = 3;

    /// The worker's spawn share as its machine sees it: cut where the
    /// machine's layout ends, the tail past it the fixture's.
    struct Cut(Rc<RefCell<SpawnPart>>);

    impl ShareWrite for Cut {
        fn byte_len(&self) -> usize {
            self.0
                .try_borrow()
                .map_or(0, |part| part.len().saturating_sub(TAIL))
        }

        fn write_with(&mut self, write: &mut dyn FnMut(&mut [u8]) -> bool) -> bool {
            let len = self.byte_len();
            self.0.try_borrow_mut().is_ok_and(|mut part| {
                part.write_with(&mut |bytes| bytes.get_mut(..len).is_some_and(&mut *write))
            })
        }
    }

    /// The native executor, saying in the share's tail that its blocks ran,
    /// and how many have.
    struct Counted {
        native: Native,
        share: Rc<RefCell<SpawnPart>>,
        runs: u64,
    }

    impl Execute for Counted {
        fn run(
            &mut self,
            code: u64,
            frame: &mut Frame,
            pages: &PageMap,
            env: &mut Env<'_>,
        ) -> Option<Exit> {
            let exit = self.native.run(code, frame, pages, env)?;
            self.runs = self.runs.saturating_add(1);
            let mut said = [1; TAIL];
            said[1..].copy_from_slice(&self.runs.to_le_bytes());
            if let Ok(mut part) = self.share.try_borrow_mut() {
                part.write_with(&mut |bytes| {
                    let at = bytes.len().checked_sub(TAIL);
                    at.and_then(|at| bytes.get_mut(at..))
                        .map(|tail| tail.copy_from_slice(&said))
                        .is_some()
                });
            }
            Some(exit)
        }

        fn adopt(&mut self, address: u64, len: u64) -> Option<u64> {
            self.native.adopt(address, len)
        }

        fn release(&mut self, code: u64) {
            self.native.release(code);
        }

        fn held(&self) -> u64 {
            self.native.held()
        }
    }

    /// The machine worker's share and executor, made from its spawn share.
    #[allow(unsafe_code)] // The executor's promise, kept as stated below.
    fn machine(part: SpawnPart) -> (Cut, Box<dyn Execute>) {
        let share = Rc::new(RefCell::new(part));
        // SAFETY: made only for the machine worker's role, whose code comes
        // over its session, whose other end is the owner that started it; the
        // owner alone installs code in it, and names only addresses
        // `code_install` answered for the blocks its compiler sandbox compiled
        // with this binary's translator, for this processor, and staged whole.
        let native = unsafe { Native::new(Box::new(roles::unmap_code)) };
        let counted = Counted {
            native,
            share: Rc::clone(&share),
            runs: 0,
        };
        (Cut(share), Box::new(counted))
    }

    /// What the owner waits for.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Stage {
        /// DOS's prompt.
        Booting,
        /// The diskette going in.
        Inserting,
        /// The program's sum.
        Running,
        /// The screen read after the machine was held, which says every
        /// block it ran has been counted.
        Holding,
    }

    /// What the owner heard, taken out of the event that lent it.
    enum Heard {
        Ready,
        Fetch {
            medium: u16,
            first: u64,
            count: u16,
        },
        Stored(Result<(), MediaError>),
        Inserted(Result<(), InsertRefusal>),
        Compile {
            request: u32,
            payload: Option<Vec<u8>>,
        },
        Screen {
            at_prompt: bool,
            answered: bool,
        },
        Unreadable,
        Gone(String),
        Nothing,
    }

    /// The machine's session as the owner drives it.
    struct Owner {
        host: Host<RtSessionChannel, LogSink>,
        fixed: SparseDisk,
        diskette: SparseDisk,
        /// The diskette's name in the session, once it is asked to go in.
        diskette_medium: Option<u16>,
        /// The line the program must print.
        sum: String,
        compiler: HeldCompiler,
        target: Target,
        installed: u32,
        stage: Stage,
        /// A screen read is out.
        looking: bool,
        /// The share, and where its tail begins.
        share: Arc<dyn Share>,
        tail: usize,
        set: WaitSet,
        members: SessionMembers,
    }

    /// A wait set, closed when dropped.
    struct WaitSet(u64);

    impl WaitSet {
        fn create() -> Result<Self, String> {
            u64::try_from(tairix_rt::waitset_create())
                .map(Self)
                .map_err(|_| String::from("pcemucheck: no wait set"))
        }
    }

    impl Drop for WaitSet {
        fn drop(&mut self) {
            let _ = tairix_rt::waitset_close(self.0);
        }
    }

    impl Owner {
        /// The default machine started on its default disk, over a session
        /// whose share has the tail past its layout, the program's diskette
        /// held to put in once DOS is up. The worker is told the memory
        /// band as the app tells it: an unreported band leaves it no room
        /// for translated code.
        fn start(target: Target) -> Result<Self, String> {
            let diskette = tairix_test_pcemu::diskette()
                .map_err(|why| format!("pcemucheck: no diskette ({why:?})"))?;
            let set = WaitSet::create()?;
            if !tairix_procinfo::pressure::watch(set.0, PRESSURE) {
                return Err(String::from(
                    "pcemucheck: the memory-pressure notice could not be watched",
                ));
            }
            let config = MACHINE.config();
            let recording = config
                .fixed
                .map(|adapter| adapter.recording)
                .ok_or("pcemucheck: the default machine has no fixed disk")?;
            let DefaultDisk {
                kind,
                low_level,
                device,
            } = default_disk(recording, SERIAL)
                .map_err(|why| format!("pcemucheck: no default disk ({why:?})"))?;
            // Whole pages, as the kernel records a region, so the worker's
            // view of its length is the owner's and both find the tail at
            // its end.
            let len = (Layout::of(&config.video).len() + TAIL).next_multiple_of(PAGE_SIZE);
            let share = NonZeroUsize::new(len).ok_or("pcemucheck: no share")?;
            let tail = len - TAIL;
            let session =
                RtSessionChannel::launch(SPAWN_SELF, WorkerMemory::Documents, Some(share))
                    .map_err(|errno| format!("pcemucheck: the worker could not start ({errno})"))?;
            let worker = session
                .pid()
                .ok_or("pcemucheck: the worker has no process")?;
            let share = session
                .share()
                .cloned()
                .ok_or("pcemucheck: the worker was handed no share")?;
            let mut seed = [0; HashSeed::LEN];
            tairix_rt::random_fill(&mut seed)
                .map_err(|errno| format!("pcemucheck: no seed ({errno})"))?;
            let setup = Setup {
                config,
                fixed: [
                    Some(FixedDisk {
                        medium: MEDIUM,
                        sectors: device.sectors(),
                        read_only: false,
                        kind,
                        format: low_level,
                    }),
                    None,
                ],
                audio_target: AUDIO_TARGET,
                room: worker_ceiling(WorkerMemory::Documents),
                seed,
                band: tairix_rt::pressure::gauge().band(),
                clock: None,
                resume: None,
            };
            let host = Host::start(session, LogSink, setup)
                .map_err(|why| format!("pcemucheck: the session was refused ({why:?})"))?;
            let members = SessionMembers::new(set.0, READABLE, WRITABLE);
            Ok(Self {
                host,
                fixed: device,
                diskette,
                diskette_medium: None,
                sum: sum_line(),
                compiler: HeldCompiler::new(0, worker),
                target,
                installed: 0,
                stage: Stage::Booting,
                looking: false,
                share,
                tail,
                set,
                members,
            })
        }

        /// Runs the machine until the program has printed its sum and the
        /// machine is held: the blocks it ran natively.
        fn run(&mut self) -> Result<u64, String> {
            let deadline = tairix_rt::clock_get().saturating_add(BUDGET_NS);
            let mut next_look = 0;
            loop {
                while let Some(heard) = self.hear() {
                    if let Some(runs) = self.act(heard)? {
                        return Ok(runs);
                    }
                }
                let now = tairix_rt::clock_get();
                if now >= deadline {
                    return Err(String::from(match self.stage {
                        Stage::Booting => "pcemucheck: DOS's prompt never came",
                        Stage::Inserting => "pcemucheck: the diskette never went in",
                        Stage::Running => "pcemucheck: the program never printed its sum",
                        Stage::Holding => "pcemucheck: the held machine's screen never came",
                    }));
                }
                if !self.looking && now >= next_look && self.stage != Stage::Holding {
                    self.look()?;
                    next_look = now.saturating_add(LOOK_NS);
                }
                self.park(next_look.min(deadline))?;
            }
        }

        /// The next event, taken out of the host.
        fn hear(&mut self) -> Option<Heard> {
            let target = self.target;
            let heard = match self.host.event()? {
                HostEvent::Ready => Heard::Ready,
                HostEvent::Fetch {
                    medium,
                    first,
                    count,
                } => Heard::Fetch {
                    medium,
                    first,
                    count,
                },
                HostEvent::Store {
                    medium: MEDIUM,
                    sector,
                    data,
                } => Heard::Stored(self.fixed.write(sector, data)),
                HostEvent::Store { .. } => Heard::Gone(String::from(
                    "pcemucheck: a store to a medium never lent for writing",
                )),
                HostEvent::Inserted {
                    drive: DRIVE,
                    medium,
                    outcome,
                } if Some(medium) == self.diskette_medium => Heard::Inserted(outcome),
                HostEvent::Inserted { .. } => {
                    Heard::Gone(String::from("pcemucheck: a diskette it never put in"))
                }
                HostEvent::Compile {
                    request,
                    model,
                    cs,
                    ip,
                    bytes,
                } => Heard::Compile {
                    request,
                    payload: Compile {
                        target,
                        model,
                        key: Key { cs, ip },
                        bytes,
                    }
                    .encode(),
                },
                HostEvent::Text(Ok(text)) => Heard::Screen {
                    at_prompt: at_prompt(&text),
                    answered: answered(&text, self.sum.as_bytes()),
                },
                HostEvent::Text(Err(_)) => Heard::Unreadable,
                HostEvent::Refused(why) => {
                    Heard::Gone(format!("pcemucheck: the machine was refused ({why:?})"))
                }
                HostEvent::Ended(how) => {
                    Heard::Gone(format!("pcemucheck: the worker ended ({how:?})"))
                }
                HostEvent::Interpreting => Heard::Gone(String::from(
                    "pcemucheck: a translated block misbehaved, so the machine fell back to the interpreter",
                )),
                _ => Heard::Nothing,
            };
            Some(heard)
        }

        /// Does what `heard` asks: the blocks the machine ran natively once
        /// it is held and its screen read.
        fn act(&mut self, heard: Heard) -> Result<Option<u64>, String> {
            let refused = |why| format!("pcemucheck: the session refused a command ({why:?})");
            match heard {
                Heard::Ready => self.host.run().map_err(refused)?,
                Heard::Fetch {
                    medium,
                    first,
                    count,
                } => {
                    let answer = read(self.medium(medium)?, first, count);
                    let answer = answer.as_deref().map_err(|&why| why);
                    self.host.answer_fetch(answer).map_err(refused)?;
                }
                Heard::Stored(answer) => self.host.answer_store(answer).map_err(refused)?,
                Heard::Inserted(Ok(())) => {
                    let mut typed = TYPED.to_vec();
                    typed.push(b'\r');
                    self.host.type_text(typed).map_err(refused)?;
                    self.stage = Stage::Running;
                }
                Heard::Inserted(Err(why)) => {
                    return Err(format!("pcemucheck: the diskette did not go in ({why:?})"));
                }
                Heard::Compile { request, payload } => {
                    let code = match payload {
                        Some(payload) => self
                            .compiler
                            .compile(&payload)
                            .map_err(|_| "pcemucheck: the compiler failed for good")?,
                        None => None,
                    };
                    self.installed += u32::from(code.is_some());
                    self.host.answer_block(request, code).map_err(refused)?;
                }
                Heard::Screen {
                    at_prompt,
                    answered,
                } => {
                    self.looking = false;
                    match self.stage {
                        // Not before: a diskette in A: at power-on is the one
                        // the machine would start from.
                        Stage::Booting if at_prompt => {
                            let form = Form::Sectors {
                                count: self.diskette.sectors(),
                                read_only: true,
                            };
                            let medium = self.host.insert(DRIVE, form).map_err(refused)?;
                            self.diskette_medium = Some(medium);
                            self.stage = Stage::Inserting;
                        }
                        Stage::Running if answered => {
                            // Read after the hold, so every block the
                            // machine ran before it is in the tail.
                            self.host.pause().map_err(refused)?;
                            self.look()?;
                            self.stage = Stage::Holding;
                        }
                        Stage::Holding => return self.native_runs().map(Some),
                        Stage::Booting | Stage::Inserting | Stage::Running => {}
                    }
                }
                // Unreadable or not, the screen's answer comes after the hold.
                Heard::Unreadable if self.stage == Stage::Holding => {
                    return self.native_runs().map(Some);
                }
                Heard::Unreadable => self.looking = false,
                Heard::Gone(why) => return Err(why),
                Heard::Nothing => {}
            }
            Ok(None)
        }

        /// The image the session names `medium`.
        fn medium(&mut self, medium: u16) -> Result<&mut SparseDisk, String> {
            if medium == MEDIUM {
                Ok(&mut self.fixed)
            } else if Some(medium) == self.diskette_medium {
                Ok(&mut self.diskette)
            } else {
                Err(String::from("pcemucheck: a medium the session never named"))
            }
        }

        fn look(&mut self) -> Result<(), String> {
            self.host
                .read_text()
                .map_err(|why| format!("pcemucheck: the screen could not be read ({why:?})"))?;
            self.looking = true;
            Ok(())
        }

        /// The blocks the worker says in the share's tail it ran natively.
        fn native_runs(&self) -> Result<u64, String> {
            let mut said = [0; TAIL];
            if !self.share.copy_out(self.tail, &mut said) {
                return Err(String::from("pcemucheck: the share holds no tail"));
            }
            let mut runs = [0; 8];
            runs.copy_from_slice(&said[1..]);
            match (said[0], u64::from_le_bytes(runs)) {
                (1, runs) if runs > 0 => Ok(runs),
                _ => Err(format!(
                    "pcemucheck: no translated block ran natively, of {} installed",
                    self.installed
                )),
            }
        }

        /// Waits for the session, or `until`.
        fn park(&mut self, until: u64) -> Result<(), String> {
            let descriptors = self.host.descriptors();
            self.members
                .sync(descriptors, self.host.wants_read(), self.host.wants_write())
                .map_err(|errno| {
                    format!("pcemucheck: the wait set refused the session ({errno})")
                })?;
            let timeout = until.saturating_sub(tairix_rt::clock_get());
            let mut token = 0;
            match tairix_rt::waitset_wait(self.set.0, timeout, &mut token) {
                0 if token == READABLE => self.host.on_readable(),
                0 if token == WRITABLE => self.host.on_writable(),
                0 if token == PRESSURE => {
                    if tairix_procinfo::pressure::refresh() {
                        self.host.pressure(tairix_rt::pressure::gauge().band());
                    }
                }
                0 => {}
                waited => match Errno::from_syscall(waited) {
                    Errno::TimedOut => {}
                    errno => return Err(format!("pcemucheck: the wait failed ({errno})")),
                },
            }
            Ok(())
        }
    }

    /// `count` sectors of `disk` from `first`.
    fn read(disk: &mut SparseDisk, first: u64, count: u16) -> Result<Vec<u8>, MediaError> {
        let mut data = Vec::new();
        data.try_reserve_exact(usize::from(count) * SECTOR)
            .map_err(|_| MediaError::Failed)?;
        let mut sector = [0; SECTOR];
        for n in 0..u64::from(count) {
            let at = first.checked_add(n).ok_or(MediaError::OutOfRange)?;
            disk.read(at, &mut sector)?;
            data.extend_from_slice(&sector);
        }
        Ok(data)
    }

    /// The checks, in turn: the line a pass prints, or why not.
    fn check() -> Result<String, String> {
        let digest = reference_run()
            .map_err(|why| format!("pcemucheck: the reference run could not be made ({why:?})"))?;
        if digest != REFERENCE_DIGEST {
            return Err(format!(
                "pcemucheck: the reference run's digest is {digest:#018x}, not {REFERENCE_DIGEST:#018x}"
            ));
        }
        let target = roles::compiled_for().map_err(|why| format!("pcemucheck: {why}"))?;
        let mut owner = Owner::start(target)?;
        let runs = owner.run()?;
        if owner.installed == 0 {
            return Err(String::from("pcemucheck: no block was installed"));
        }
        Ok(pass_line(owner.installed, runs))
    }

    /// Report `reason` and park, so the vertical's end, armed on this
    /// program's exit, never fires; exit only if even the park is refused.
    fn fail(reason: &str) -> i32 {
        write_stderr_line(reason);
        let _ = tairix_rt::park_forever();
        write_stderr_line("pcemucheck: parking was refused");
        1
    }

    fn main() -> i32 {
        if let Some(exit) = roles::served(machine) {
            return exit;
        }
        match check() {
            Ok(line) if Stdout.write_all(line.as_bytes()).is_ok() => 0,
            Ok(_) => fail("pcemucheck: the pass could not be reported"),
            Err(reason) => fail(&reason),
        }
    }

    tairix_rt::entry!(main);
}

#[cfg(not(all(freestanding, feature = "program")))]
fn main() {}

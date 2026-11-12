//! The sandbox roles a program running machines starts its own binary in —
//! a machine's worker and a compiler — and the owner's hold on a machine's
//! compiler, which installs the code it stages into the worker. `Run` plays
//! them, and so does the program the cross-target verticals run.

use alloc::boxed::Box;
use alloc::vec::Vec;

use tairix_abi::Errno;
use tairix_log::DiscardSink;
use tairix_reclaim::{PressureBand, PressureGauge};
use tairix_rt::shm::SpawnPart;
use tairix_rt::LogSink;
use tairix_sandbox::rt::{
    serve_stdio, serves, session_worker_role, spawn_share, worker_role, RtChannel, RtLauncher,
    StdioChannel,
};
use tairix_sandbox::session::ShareWrite;
use tairix_sandbox::{
    ParserSandbox, SandboxError, Service, Unbelieved, WorkerExit, WorkerLink, WorkerMemory,
};

use crate::cpu::jit::compiler::{self, Compiled};
use crate::cpu::jit::dispatch::Execute;
use crate::link::{Installed, WORKER_INBOUND};
use crate::worker::{self, Memory, SandboxLink};

/// The one-shot service a compiler sandbox is started to serve.
const COMPILER_SERVICE: &[u8] = b"compiler";

/// How long a compiler has to answer one block: a containment bound, many
/// times the heaviest block's compile on the slowest core.
const COMPILE_DEADLINE_NS: u64 = 10_000_000_000;

/// Failures of a machine's compilers after which its blocks are left to the
/// interpreter: a block that crashes the compiler is a defect of the
/// compiler's, and three of them say it will not compile this guest.
const COMPILER_STRIKES: u32 = 3;

/// Exit code of a machine worker its front-end left or broke.
const EXIT_WORKER_ENDED: i32 = 1;

/// Where the machine worker's containment is logged: nowhere, as a
/// canonical sandbox has no log of its own.
static DISCARD: DiscardSink = DiscardSink;

/// Serves the role this process was started in, a compiler's or a machine
/// worker's, the worker writing into and running its blocks on what
/// `machine` makes of its spawn share: the exit to end with, or none for the
/// owner.
pub fn served<S: ShareWrite>(
    machine: impl FnOnce(SpawnPart) -> (S, Box<dyn Execute>),
) -> Option<i32> {
    if worker_role() {
        return Some(if serves(COMPILER_SERVICE) {
            serve_stdio(&mut CompilerService).exit_code()
        } else {
            WorkerExit::Unserved.code()
        });
    }
    session_worker_role().then(|| match spawn_share() {
        Ok(share) => {
            let (share, executor) = machine(share);
            serve_machine(share, executor)
        }
        Err(_) => EXIT_WORKER_ENDED,
    })
}

/// The processor blocks are compiled for, with the extensions every core
/// this process may run on has.
///
/// # Errors
///
/// Why none is, where this session may not install code or there is no
/// backend for the processor.
pub fn compiled_for() -> Result<tairix_codegen::Target, &'static str> {
    if !tairix_rt::cap_query(tairix_abi::CapabilityId::JIT_MAP_EXEC) {
        return Err("this session may not install compiled code");
    }
    let facts = tairix_rt::boot_facts().map_err(|_| "the processor could not be told")?;
    tairix_codegen::Target::of(facts.arch, tairix_rt::cpu_features())
        .ok_or("no code is generated for this processor")
}

/// Gives back `len` bytes of code installed at `address`: how a native
/// executor releases a block.
pub fn unmap_code(address: u64, len: u64) {
    let _ = tairix_rt::mem_unmap(address, usize::try_from(len).unwrap_or(usize::MAX));
}

/// The worker's memory-pressure readings: its own process's.
struct RtMemory;

impl Memory for RtMemory {
    fn gauge(&self) -> &'static (dyn PressureGauge + 'static) {
        tairix_rt::pressure::gauge()
    }

    fn report(&self, band: PressureBand) {
        tairix_rt::pressure::report(band);
    }
}

/// Serves the machine over the wired standard streams.
fn serve_machine(share: impl ShareWrite, executor: Box<dyn Execute>) -> i32 {
    let Ok(channel) = WorkerLink::new(StdioChannel, WORKER_INBOUND) else {
        return EXIT_WORKER_ENDED;
    };
    let link = SandboxLink::new(channel, tairix_rt::clock_get);
    match worker::serve(link, share, (RtMemory, executor), &DISCARD) {
        worker::port::End::Closed => 0,
        _ => EXIT_WORKER_ENDED,
    }
}

/// Compiles the blocks the front-end sends, staging each one's code for it
/// to install.
struct CompilerService;

impl Service for CompilerService {
    fn handle(&mut self, request: &[u8]) -> Vec<u8> {
        compiler::answer(request, |code| tairix_rt::stage_code(code).ok())
    }
}

/// A machine's blocks are left to the interpreter from now on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interpret;

/// Why a block's compile and install came to nothing.
enum Failed {
    Sandbox(SandboxError),
    /// The compiler answered what cannot be believed.
    Unbelievable,
    /// The worker has no room for the code now.
    NoRoom,
    /// The platform installs no code at all.
    Platform,
}

impl Unbelieved for Failed {
    fn unbelieved(&self) -> bool {
        matches!(self, Self::Unbelievable)
    }
}

/// A machine's compiler: the sandbox, the machine's worker it installs
/// into, and how often it has failed.
pub struct HeldCompiler {
    machine: u64,
    worker: i64,
    sandbox: ParserSandbox<RtLauncher, LogSink>,
    strikes: u32,
    /// It failed too often, or none could be started: the machine
    /// interprets.
    broken: bool,
}

impl HeldCompiler {
    /// The compiler of machine `machine`, whose worker is process `worker`;
    /// its sandbox starts with the first block.
    #[must_use]
    pub fn new(machine: u64, worker: i64) -> Self {
        let launcher = RtLauncher::own_binary(WorkerMemory::Messages)
            .serving(COMPILER_SERVICE)
            .answering_within(COMPILE_DEADLINE_NS);
        Self {
            machine,
            worker,
            sandbox: ParserSandbox::new(launcher, LogSink),
            strikes: 0,
            broken: false,
        }
    }

    /// The machine it compiles for.
    #[must_use]
    pub const fn machine(&self) -> u64 {
        self.machine
    }

    /// Block `payload` compiled and installed in the worker: where, or none
    /// where it was not.
    ///
    /// # Errors
    ///
    /// [`Interpret`] once the compiler has failed for good: the machine's
    /// blocks are the interpreter's from then on.
    pub fn compile(&mut self, payload: &[u8]) -> Result<Option<Installed>, Interpret> {
        if self.broken {
            return Err(Interpret);
        }
        let worker = self.worker;
        let installed = self.sandbox.ask(|sandbox| {
            let reply = sandbox.request(payload).map_err(Failed::Sandbox)?;
            let (base, len) = match Compiled::decode(&reply) {
                Ok(Compiled::Staged { base, len }) => (base, len),
                Ok(Compiled::Refused) => return Ok(None),
                Err(_) => return Err(Failed::Unbelievable),
            };
            let compiler = sandbox
                .live()
                .map(RtChannel::pid)
                .ok_or(Failed::Unbelievable)?;
            let bytes = usize::try_from(len).map_err(|_| Failed::Unbelievable)?;
            match tairix_rt::code_install(compiler, worker, base, bytes) {
                Ok(address) => Ok(Some(Installed { address, len })),
                Err(Errno::OutOfMemory | Errno::OutOfRange | Errno::MediumError) => {
                    Err(Failed::NoRoom)
                }
                Err(Errno::DeviceFault) => Err(Failed::Platform),
                Err(_) => Err(Failed::Unbelievable),
            }
        });
        match installed {
            Ok(code) => Ok(code),
            Err(Failed::Sandbox(SandboxError::OutOfMemory)) => Ok(None),
            Err(Failed::NoRoom) => {
                // The code it staged goes with it.
                self.sandbox.release();
                Ok(None)
            }
            Err(Failed::Platform | Failed::Sandbox(SandboxError::WorkerUnavailable(_))) => {
                self.give_up()
            }
            Err(Failed::Sandbox(_) | Failed::Unbelievable) => {
                self.strikes += 1;
                if self.strikes >= COMPILER_STRIKES {
                    self.give_up()
                } else {
                    Ok(None)
                }
            }
        }
    }

    fn give_up(&mut self) -> Result<Option<Installed>, Interpret> {
        self.broken = true;
        self.sandbox.release();
        Err(Interpret)
    }
}

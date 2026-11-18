//! The kernel's core (`plans/RISCOS-EMULATOR.md` §4): the session's
//! processors, the calls the kernel makes into guest code and the run that
//! drives them, and [`Os`], the view every SWI and built-in module works
//! through.
//!
//! The kernel is Rust entered at a trap: a `SWI`, or a `UDF` fetched from
//! the host pages. Guest code the kernel calls — a vector claimant, a
//! module's entry, a handler — runs nested inside the Rust that called it,
//! to a bounded depth. Control that leaves for somewhere that never returns
//! (an error handler, an exit, a new program) unwinds every nested call
//! first, as RISC OS flattens its supervisor stack.

mod access;
mod call;
mod processor;

use alloc::boxed::Box;
use alloc::vec::Vec;

use tairix_riscos_arm::cpu::State;
use tairix_riscos_arm::psr;

pub use call::{Ended, Entry, Returned, Target, Transfer, Unwind, Wait};
pub(crate) use processor::Processor;

use crate::memory::layout::Layout;
use crate::memory::policy::Ceiling;
use crate::memory::{Bits, Memory, MemoryError};

/// The registers a SWI, a vector claimant or a handler is called with, and
/// answers in: R0 to R12 and the condition flags.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Regs {
    /// R0 to R12.
    pub r: [u32; 13],
    /// N, Z, C and V at their CPSR positions; nothing else.
    pub flags: u32,
}

impl Regs {
    /// R0 to R12 as given, every flag clear.
    #[must_use]
    pub const fn new(r: [u32; 13]) -> Self {
        Self { r, flags: 0 }
    }

    /// Whether `flag` (one of [`psr::N`], [`psr::Z`], [`psr::C`] and
    /// [`psr::V`]) is set.
    #[must_use]
    pub const fn flag(&self, flag: u32) -> bool {
        self.flags & flag != 0
    }

    /// Sets or clears `flag`.
    pub const fn set_flag(&mut self, flag: u32, on: bool) {
        if on {
            self.flags |= flag;
        } else {
            self.flags &= !flag;
        }
        self.flags &= psr::NZCV;
    }

    /// R`n`'s low byte: all a byte-wide parameter reads.
    #[must_use]
    pub const fn byte(&self, n: usize) -> u8 {
        self.r[n].to_le_bytes()[0]
    }
}

/// A length, count or index the kernel holds, as a guest word: what the
/// kernel builds or reads is bounded far below 4 GiB, and saturating keeps
/// the conversion total. A constant's offsets are made with it too.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a length past what a word holds is made the most it holds first"
)]
#[must_use]
pub const fn len32(len: usize) -> u32 {
    if len >= u32::MAX as usize {
        u32::MAX
    } else {
        len as u32
    }
}

/// What the host says between slices of a long run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pulse {
    /// Carry on.
    Continue,
    /// Stop the run: the context is to be stopped.
    Stop,
}

/// What the kernel asks of the session hosting it. The kernel is a pure
/// function of the bytes it is given, its input and these readings, so a
/// session replays exactly.
pub trait Host {
    /// The guest's monotonic clock: centiseconds since the session began.
    fn monotonic(&mut self) -> u64;

    /// The host's real time: centiseconds since 00:00:00 on 1 January 1900,
    /// UTC, the base of RISC OS's five-byte time.
    fn real_time(&mut self) -> u64;

    /// Said between slices of a run that goes on; [`Pulse::Stop`] ends it.
    fn pulse(&mut self) -> Pulse;

    /// The guest has nothing to do before the monotonic clock reaches
    /// `until`, or ever if none: the host waits until then or until it has
    /// something for the guest. [`Pulse::Stop`] ends the run.
    fn wait(&mut self, until: Option<u64>) -> Pulse;

    /// The next input event the host holds for the guest, if any.
    fn input(&mut self) -> Option<crate::input::InputEvent>;

    /// `request` carried to the side holding the session's files, its reply
    /// appended to `reply`; false where the session has no such side or the
    /// relay failed, which the guest sees as a filing system with no discs.
    fn relay(&mut self, request: &[u8], reply: &mut Vec<u8>) -> bool;
}

/// What a session is made with.
pub struct Config {
    /// The address space's layout.
    pub layout: &'static Layout,
    /// The RAM the guest has.
    pub ceiling: Ceiling,
    /// Decoded pages each processor keeps: the memory policy's share for
    /// them.
    pub memo_pages: usize,
    /// The host.
    pub host: Box<dyn Host>,
    /// The VDU drivers, if the session has them.
    pub vdu: Option<crate::output::VduDrivers>,
    /// The built-in modules the session's other crates provide, besides the
    /// kernel's own.
    pub modules: &'static [&'static crate::module::Builtin],
    /// The TAIRiX locale's time zone, which the territory's zone is.
    pub zone: tairix_tz::Zone<'static>,
    /// The machine's memory pressure, which the session's caches give way
    /// to.
    pub pressure: &'static (dyn tairix_reclaim::PressureGauge + 'static),
    /// Where the session's caches record what they decide.
    pub log: &'static (dyn tairix_log::Sink + Sync),
}

/// The control register's soft copy at reset: the MMU, caches and write
/// buffer on.
const MMU_CONTROL: u32 = 0x00C5_387F;
/// MEMC's soft copy at reset: video on.
const MEMC: u32 = 0x0360_0400;
/// The system heap's size at reset.
const SYSTEM_HEAP_START: u32 = 32 * 1024;
/// The RMA's size at reset.
const RMA_START: u32 = 4 * 1024;

/// A SWI's or a call's failure.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fail {
    /// An error: the address of its block in guest memory.
    Error(u32),
    /// Control leaves for somewhere that does not return here.
    Unwind(Unwind),
}

impl From<Unwind> for Fail {
    fn from(unwind: Unwind) -> Self {
        Self::Unwind(unwind)
    }
}

/// Why a run returned to the session.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// The instruction budget is spent; the context may run again.
    Budget,
    /// The context waits for what is named; it runs again when that comes.
    Waiting(Wait),
    /// The host stopped it.
    Stopped,
    /// Its program ended.
    Ended(Ended),
    /// It reached something the processor does not model, which the front
    /// end names.
    Unmodelled(tairix_riscos_arm::cpu::Unmodelled),
}

/// The outermost SWI from guest code being performed, or the exception
/// whose error is raised: the SWI's number, where its caller resumes and its
/// R10 to R12, which an error handler is given.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Frame {
    pub(crate) swi: Option<u32>,
    pub(crate) return_pc: u32,
    pub(crate) upper: [u32; 3],
}

/// The last abort the kernel did not expect, for `OS_ReadSysInfo` 7.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Abort {
    pub(crate) pc: u32,
    pub(crate) psr: u32,
    pub(crate) address: u32,
}

/// The kernel's fixed strings in guest memory, for `OS_ReadSysInfo` 9.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Strings {
    pub(crate) os_name: u32,
    pub(crate) build_date: u32,
    pub(crate) platform: u32,
}

/// The string `OS_GSInit` began, held for `OS_GSRead`.
pub(crate) struct GsState {
    pub(crate) base: u32,
    pub(crate) text: alloc::vec::Vec<u8>,
    pub(crate) flags: tairix_obey::gstrans::Flags,
    pub(crate) progress: tairix_obey::gstrans::Progress,
    pub(crate) generation: u32,
}

/// The kernel's scratch buffer in guest memory: room for the strings and
/// blocks it hands the SWIs it calls.
pub(crate) const SCRATCH: usize = 256;

/// The kernel's message file's name, which its descriptor keeps; the
/// descriptor, the name and the room its lookups are asked in.
const KERNEL_MESSAGES: &[u8] = b"Resources:$.Resources.Kernel.Messages\0";
const KERNEL_MESSAGES_REQUEST: u32 =
    (crate::msgtrans::OWN_DESCRIPTOR + len32(KERNEL_MESSAGES.len())).next_multiple_of(4);
const KERNEL_MESSAGES_ROOM: u32 = KERNEL_MESSAGES_REQUEST + crate::msgtrans::REQUEST;

/// Everything the kernel holds but the address space: the processors and
/// the kernel's own state. Kept apart from [`Memory`] so the address space
/// can hand itself to the code a change of it calls.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent switches of the run"
)]
pub struct World {
    pub(crate) processor: Processor,
    pub(crate) host: Box<dyn Host>,
    /// Where the kernel's host routines start.
    pub(crate) routines: u32,
    /// Guest calls in progress, each nested inside the Rust that made it.
    pub(crate) depth: u32,
    /// SWIs the kernel's own Rust is performing within the program's: an
    /// input wait inside one cannot have the program's SWI performed again,
    /// which has run past its start.
    pub(crate) kernel_calls: u32,
    /// The address of the `SWI` being performed, where an access it makes
    /// that aborts is reported.
    pub(crate) swi_at: u32,
    /// The OS SWI being performed.
    pub(crate) performing: u32,
    /// The outermost SWI from guest code being performed.
    pub(crate) frame: Option<Frame>,
    /// Where an unwinding transfer goes.
    pub(crate) transfer: Option<Transfer>,
    /// An unwind a call through the address space began, for after it.
    pub(crate) unwinding: Option<Unwind>,
    /// The error handler's transfer runs the callbacks due first.
    pub(crate) callbacks_first: bool,
    /// A `SWI` passed on by guest code that claimed the SWI vector is being
    /// performed.
    pub(crate) swi_passing: bool,
    pub(crate) in_interrupt: bool,
    pub(crate) env: crate::env::Environment,
    pub(crate) vectors: crate::vectors::Vectors,
    pub(crate) callbacks: crate::callbacks::Callbacks,
    pub(crate) clock: crate::clock::Clock,
    pub(crate) events: crate::events::Events,
    pub(crate) modules: crate::module::Modules,
    pub(crate) cli: crate::cli::State,
    pub(crate) vars: crate::vars::Index,
    pub(crate) input: crate::input::Input,
    pub(crate) cmos: crate::cmos::Cmos,
    pub(crate) errors: crate::error::Ring,
    pub(crate) titles: crate::dynamic::Titles,
    pub(crate) traps: alloc::vec::Vec<crate::traps::Trap>,
    pub(crate) devices: alloc::vec::Vec<u32>,
    pub(crate) last_abort: Abort,
    pub(crate) strings: Strings,
    pub(crate) scratch: u32,
    pub(crate) mmu_control: u32,
    pub(crate) memc: u32,
    pub(crate) gs: Option<GsState>,
    pub(crate) gs_generation: u32,
    /// What the SWI a wait unwound out of had done, for its performance
    /// again.
    pub(crate) continuation: Option<crate::input::Continuation>,
    /// That SWI's address and number, which the context performs again.
    pub(crate) resume: Option<(u32, u32)>,
    /// Whether guest code has run during the outermost SWI, which may then
    /// not be performed again from its start.
    pub(crate) guest_called: bool,
    /// The processor's work since the host last had its word.
    pub(crate) since_word: u64,
    pub(crate) vdu: Option<crate::output::VduDrivers>,
    /// The serial output stream's handle, and the printer stream's.
    pub(crate) serial_out: u32,
    pub(crate) printer: u32,
    pub(crate) resource_fs: crate::resourcefs::State,
    pub(crate) message_trans: crate::msgtrans::State,
    /// The ROM's resource files' block in the host pages.
    pub(crate) rom_resources: u32,
    /// The kernel's dictionary and MessageTrans's in the host pages.
    pub(crate) dictionaries: crate::dictionary::Dictionaries,
    /// The kernel's own message file, which its errors are looked up in.
    pub(crate) kernel_messages: crate::msgtrans::Own,
    /// An error is being looked up: one it raises takes the kernel's own
    /// text.
    pub(crate) translating: bool,
    pub(crate) territory: crate::territory::Manager,
    pub(crate) international: crate::international::State,
    pub(crate) intkey: crate::intkey::State,
    pub(crate) country: crate::byte::Country,
    pub(crate) keyboard: crate::keyboard::Keyboard,
    pub(crate) uk: crate::territory::uk::State,
    pub(crate) zone: tairix_tz::Zone<'static>,
    pub(crate) pressure: &'static (dyn tairix_reclaim::PressureGauge + 'static),
    pub(crate) log: &'static (dyn tairix_log::Sink + Sync),
    pub(crate) extensions: crate::extension::Extensions,
}

impl World {
    /// A kernel routine's address on the host pages.
    pub(crate) const fn routine_address(&self, routine: crate::host::Routine) -> u32 {
        self.routines + 4 * routine.number()
    }
}

/// The kernel and the address space it runs in, for one session.
pub struct Kernel {
    memory: Memory,
    world: World,
}

/// The kernel as a SWI or a built-in module sees it: the address space and
/// everything else, apart, so either can be handed to code the other calls.
pub struct Os<'a> {
    pub(crate) memory: &'a mut Memory,
    pub(crate) world: &'a mut World,
}

/// Why a session could not be made.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BootError {
    /// The address space refused.
    Memory(MemoryError),
    /// Guest memory the kernel needs for itself was refused.
    NoMemory,
    /// A built-in module of this title is no provided module of the
    /// inventory, is given twice, or would not start.
    Module(&'static str),
    /// The processor refused the state a reset leaves it in.
    Processor,
}

impl From<MemoryError> for BootError {
    fn from(error: MemoryError) -> Self {
        Self::Memory(error)
    }
}

impl Kernel {
    /// A session's kernel: its address space laid out, its heaps made, its
    /// state as RISC OS 5's is at reset, with no task yet.
    pub fn new(config: Config) -> Result<Self, BootError> {
        let mut memory = Memory::new(config.layout, config.ceiling)?;
        let builtins = crate::module::arranged(config.modules)?;
        let routines = crate::host::install(&mut memory, crate::module::entry_count(&builtins))?;
        let address = |routine: crate::host::Routine| routines + 4 * routine.number();
        let installed = crate::module::install(&mut memory, &builtins, |entry| {
            address(crate::host::Routine::Builtin(entry))
        })?;
        let rom_resources = crate::resources::install(&mut memory, &builtins)?;
        let dictionaries = crate::dictionary::install(&mut memory)?;
        let bits = if config.layout.admits_26bit {
            Bits::Bit26
        } else {
            Bits::Bit32
        };
        let placeholder = crate::env::Buffers {
            error: 0,
            dump: 0,
            command: 0,
            start: 0,
        };
        let mut host = config.host;
        let locale = crate::territory::uk::locale(config.zone, host.real_time());
        let world = World {
            processor: Processor::new(bits, config.memo_pages),
            host,
            routines,
            depth: 0,
            kernel_calls: 0,
            swi_at: 0,
            performing: 0,
            frame: None,
            transfer: None,
            unwinding: None,
            callbacks_first: false,
            swi_passing: false,
            in_interrupt: false,
            env: crate::env::Environment::new(
                placeholder,
                |handler| address(crate::host::Routine::Handler(handler)),
                crate::memory::layout::APPLICATION,
            ),
            vectors: crate::vectors::Vectors::new(|vector| {
                address(crate::host::Routine::Vector(vector))
            })?,
            callbacks: crate::callbacks::Callbacks::new(),
            clock: crate::clock::Clock::new(),
            events: crate::events::Events::new(),
            modules: crate::module::Modules::new(),
            cli: crate::cli::State::new(),
            vars: crate::vars::Index::new(),
            input: crate::input::Input::new()?,
            cmos: crate::cmos::Cmos::new(locale),
            errors: crate::error::Ring::at(0),
            titles: crate::dynamic::Titles::new(),
            traps: alloc::vec::Vec::new(),
            devices: alloc::vec::Vec::new(),
            last_abort: Abort::default(),
            strings: Strings::default(),
            scratch: 0,
            mmu_control: MMU_CONTROL,
            memc: MEMC,
            gs: None,
            gs_generation: 0,
            continuation: None,
            resume: None,
            guest_called: false,
            since_word: 0,
            vdu: config.vdu,
            serial_out: 0,
            printer: 0,
            resource_fs: crate::resourcefs::State::new(),
            message_trans: crate::msgtrans::State::new(),
            rom_resources,
            dictionaries,
            kernel_messages: crate::msgtrans::Own::new(),
            translating: false,
            territory: crate::territory::Manager::new(),
            international: crate::international::State::new(),
            intkey: crate::intkey::State::new(),
            country: crate::byte::Country::new(),
            keyboard: crate::keyboard::Keyboard::new(),
            uk: crate::territory::uk::State::new(),
            zone: config.zone,
            pressure: config.pressure,
            log: config.log,
            extensions: crate::extension::Extensions::new(),
        };
        let mut kernel = Self { memory, world };
        kernel.world.modules.roms = installed.roms;
        kernel.world.modules.entries = installed.entries;
        kernel.world.modules.empty = installed.empty;
        kernel.boot()?;
        Ok(kernel)
    }

    /// The rest of a reset: the heaps, the kernel's buffers in them, the
    /// environment, the vectors' defaults, zero page and the variables.
    fn boot(&mut self) -> Result<(), BootError> {
        let layout = self.memory.layout();
        let heap_handler = self.world.routine_address(crate::host::Routine::HeapArea);
        for (heap, range, title, size) in [
            (
                crate::kheap::KernelHeap::System,
                layout.system_heap.clone(),
                &b"System heap"[..],
                SYSTEM_HEAP_START,
            ),
            (
                crate::kheap::KernelHeap::Rma,
                layout.rma.clone(),
                &b"Module area"[..],
                RMA_START,
            ),
        ] {
            let handler = crate::memory::Handler {
                code: heap_handler,
                workspace: heap.area(),
                bits: Bits::Bit32,
            };
            self.memory
                .create_system_area(heap.area(), range, title, handler, size, &mut self.world)
                .map_err(|_| BootError::NoMemory)?;
            let mut os = self.os();
            os.initialise_heap(heap, size)
                .map_err(|_| BootError::NoMemory)?;
        }
        let mut os = self.os();
        let claim =
            |os: &mut Os<'_>, bytes: u32| os.system_node(bytes).map_err(|_| BootError::NoMemory);
        let ring = claim(&mut os, crate::error::BUFFERS * crate::error::BUFFER_SIZE)?;
        os.world.errors = crate::error::Ring::at(ring);
        let buffers = crate::env::Buffers {
            error: claim(&mut os, crate::env::ERROR_BUFFER)?,
            dump: claim(&mut os, crate::env::DUMP)?,
            command: claim(&mut os, crate::env::COMMAND_LINE)?,
            start: claim(&mut os, 8)?,
        };
        os.world.scratch = claim(&mut os, len32(SCRATCH))?;
        let ring = claim(&mut os, crate::cli::State::size())?;
        let expansion = claim(&mut os, len32(crate::cli::LONGEST))?;
        os.world.cli = crate::cli::State::at(ring, expansion);
        let routines = os.world.routines;
        os.world.env = crate::env::Environment::new(
            buffers,
            |handler| routines + 4 * crate::host::Routine::Handler(handler).number(),
            crate::memory::layout::APPLICATION,
        );
        for (slot, text) in [
            (0, crate::release::OS_NAME),
            (1, crate::release::BUILD_DATE),
            (2, crate::release::PLATFORM),
        ] {
            let at = claim(&mut os, len32(text.len()) + 1)?;
            os.write_terminated(at, text, 0, text.len() + 1)
                .map_err(|_| BootError::NoMemory)?;
            match slot {
                0 => os.world.strings.os_name = at,
                1 => os.world.strings.build_date = at,
                _ => os.world.strings.platform = at,
            }
        }
        os.write_byte(buffers.command, 0)
            .map_err(|_| BootError::NoMemory)?;
        os.reset_byte_vars();
        os.world.vars.buffer = claim(&mut os, crate::vars::preset::BUFFER)?;
        os.set_presets().map_err(|_| BootError::NoMemory)?;
        let messages = claim(&mut os, KERNEL_MESSAGES_ROOM)?;
        os.write_bytes(messages + crate::msgtrans::OWN_DESCRIPTOR, KERNEL_MESSAGES)
            .map_err(|_| BootError::NoMemory)?;
        os.world.kernel_messages = crate::msgtrans::Own::at(
            messages,
            messages + crate::msgtrans::OWN_DESCRIPTOR,
            messages + KERNEL_MESSAGES_REQUEST,
        );
        let keyboard = claim(&mut os, crate::keyboard::HANDLER_WORKSPACE + 4)?;
        os.write_word(keyboard + crate::keyboard::HANDLER_WORKSPACE, 0)
            .map_err(|_| BootError::NoMemory)?;
        os.world.keyboard.workspace = keyboard;
        os.world.keyboard.empty = keyboard + crate::keyboard::HANDLER_WORKSPACE;
        os.world.keyboard.exits = os.world.routine_address(crate::host::Routine::KeyExit(0));
        // Before any module's code can run on them.
        let state = os.world.processor.state();
        let state = os.emptied_stacks(state);
        let bits = os.world.processor.bits();
        os.world
            .processor
            .load(bits, &state)
            .map_err(|_| BootError::Processor)?;
        os.create_ram_disc().map_err(|_| BootError::NoMemory)?;
        os.start_roms()?;
        os.start_country().map_err(|_| BootError::NoMemory)?;
        os.keyboard_arrives().map_err(|_| BootError::NoMemory)?;
        // The modules' callbacks run before the kernel's own message file is
        // opened.
        os.run_callbacks().map_err(|_| BootError::NoMemory)?;
        os.open_kernel_messages().map_err(|_| BootError::NoMemory)
    }

    /// The view SWIs work through.
    pub fn os(&mut self) -> Os<'_> {
        Os {
            memory: &mut self.memory,
            world: &mut self.world,
        }
    }

    /// Runs the current context for at most `budget` instructions, from the
    /// program a host call left it to where one did.
    pub fn run(&mut self, budget: u64) -> Outcome {
        self.os().run(budget)
    }

    /// The current processor's registers.
    pub fn state(&mut self) -> State {
        self.world.processor.state()
    }

    /// The profile the current code runs under.
    #[must_use]
    pub const fn bits(&self) -> Bits {
        self.world.processor.bits()
    }

    /// The address space.
    pub const fn memory(&mut self) -> &mut Memory {
        &mut self.memory
    }
}

#[cfg(test)]
mod tests;

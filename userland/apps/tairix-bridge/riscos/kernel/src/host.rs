//! The host pages' kernel routines (`plans/RISCOS-EMULATOR.md` decision 5):
//! each is one `UDF` whose immediate names it, at an address the guest can
//! call or be handed, and the processor vectors in zero page that load
//! their entries.

use alloc::vec::Vec;

use tairix_riscos_arm::cpu::Exception;
use tairix_riscos_arm::instruction::{CallOp, Op};
use tairix_riscos_arm::reg::Reg;

use crate::env::HANDLERS;
use crate::memory::{Memory, MemoryError};
use crate::module::{instruction, jump_through};
use crate::os::{Os, Unwind};
use crate::vectors::COUNT as VECTORS;
use crate::zeropage;

/// The processor vectors.
const SLOTS: u32 = 8;

/// The exceptions in vector order, a word apart from zero page's start:
/// reset, undefined instruction, SWI, prefetch abort, data abort, the
/// address exception's unused slot, IRQ and FIQ.
pub(crate) const EXCEPTIONS: [Option<Exception>; SLOTS as usize] = [
    Some(Exception::Reset),
    Some(Exception::Undefined),
    Some(Exception::Swi),
    Some(Exception::PrefetchAbort),
    Some(Exception::DataAbort),
    None,
    Some(Exception::Irq),
    Some(Exception::Fiq),
];

/// A kernel routine on the host pages.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Routine {
    /// Where a call's code returns.
    Return,
    /// Where a vector claimant passes the call on.
    PassOn,
    /// The address a vector's caller stacks, which a claimant claims the
    /// call through.
    Claimed,
    /// The processor vector's entry at this slot of [`EXCEPTIONS`].
    Exception(u32),
    /// An environment handler's default.
    Handler(u32),
    /// A software vector's default claimant.
    Vector(u32),
    /// The system heap's and the RMA's area handler, R12 saying which.
    HeapArea,
    /// The RAM disc's area handler.
    RamDiscArea,
    /// `OS_PlatformFeatures` 35's routine: the exclusive monitor cleared.
    ClearExclusive,
    /// An `ARMop` `OS_MMUControl` 2 names: cache, TLB and barrier work.
    ArmOp(u32),
    /// `OS_TaskControl`'s routine: the privileged stacks emptied.
    ResetStacks,
    /// The output redirection's `WrchV` claimant.
    RedirectOutput,
    /// An entry of one of the kernel's code variables: the variable this
    /// many in, its write entry then its read.
    Variable(u32),
    /// A key handler's exit: the mouse's buttons, then Break, a word apart.
    KeyExit(u32),
    /// One of the VDU drivers' routines, by its place among them.
    Vdu(u32),
    /// The entry of a built-in module's image this many after the first,
    /// which enters its Rust.
    Builtin(u32),
}

/// The `ARMops` `OS_MMUControl` 2 gives addresses for.
pub(crate) const ARM_OPS: u32 = 22;
/// The `ARMop` that answers the size past which whole-cache work is cheaper:
/// the level 1 data cache's.
const CACHE_RANGE_THRESHOLD: u32 = 3;

const EXCEPTION_BASE: u32 = 3;
const HANDLER_BASE: u32 = EXCEPTION_BASE + SLOTS;
const VECTOR_BASE: u32 = HANDLER_BASE + HANDLERS;
const HEAP_AREA: u32 = VECTOR_BASE + VECTORS;
const RAM_DISC_AREA: u32 = HEAP_AREA + 1;
const CLEAR_EXCLUSIVE: u32 = RAM_DISC_AREA + 1;
const ARM_OP_BASE: u32 = CLEAR_EXCLUSIVE + 1;
const RESET_STACKS: u32 = ARM_OP_BASE + ARM_OPS;
const REDIRECT_OUTPUT: u32 = RESET_STACKS + 1;
const VARIABLE_BASE: u32 = REDIRECT_OUTPUT + 1;
const KEY_EXIT_BASE: u32 = VARIABLE_BASE + crate::vars::preset::ENTRIES;
/// A key handler's exits: the mouse's buttons and Break.
const KEY_EXITS: u32 = 2;
const VDU_BASE: u32 = KEY_EXIT_BASE + KEY_EXITS;
/// The first built-in module entry's routine: the kernel's own are below it.
const BUILTIN_BASE: u32 = VDU_BASE + crate::output::VDU_ROUTINES;

/// The FIQ vector's slot in [`EXCEPTIONS`].
pub(crate) const FIQ: u32 = 7;
/// The undefined instruction vector's slot in [`EXCEPTIONS`].
pub(crate) const UNDEFINED: u32 = 1;
const _: () = assert!(matches!(EXCEPTIONS[FIQ as usize], Some(Exception::Fiq)));
const _: () = assert!(matches!(
    EXCEPTIONS[UNDEFINED as usize],
    Some(Exception::Undefined)
));

impl Routine {
    /// The routine a host page's `UDF #number` is: the kernel installs no
    /// other `UDF` there, so every number names one.
    pub(crate) const fn of(number: u32) -> Self {
        match number {
            0 => Self::Return,
            1 => Self::PassOn,
            2 => Self::Claimed,
            n if n < HANDLER_BASE => Self::Exception(n - EXCEPTION_BASE),
            n if n < VECTOR_BASE => Self::Handler(n - HANDLER_BASE),
            n if n < HEAP_AREA => Self::Vector(n - VECTOR_BASE),
            HEAP_AREA => Self::HeapArea,
            RAM_DISC_AREA => Self::RamDiscArea,
            CLEAR_EXCLUSIVE => Self::ClearExclusive,
            n if n < RESET_STACKS => Self::ArmOp(n - ARM_OP_BASE),
            RESET_STACKS => Self::ResetStacks,
            REDIRECT_OUTPUT => Self::RedirectOutput,
            n if n < KEY_EXIT_BASE => Self::Variable(n - VARIABLE_BASE),
            n if n < VDU_BASE => Self::KeyExit(n - KEY_EXIT_BASE),
            n if n < BUILTIN_BASE => Self::Vdu(n - VDU_BASE),
            n => Self::Builtin(n - BUILTIN_BASE),
        }
    }

    /// The routine's `UDF` immediate, which is also its word's index in the
    /// kernel's host block.
    pub(crate) const fn number(self) -> u32 {
        match self {
            Self::Return => 0,
            Self::PassOn => 1,
            Self::Claimed => 2,
            Self::Exception(slot) => EXCEPTION_BASE + slot,
            Self::Handler(handler) => HANDLER_BASE + handler,
            Self::Vector(vector) => VECTOR_BASE + vector,
            Self::HeapArea => HEAP_AREA,
            Self::RamDiscArea => RAM_DISC_AREA,
            Self::ClearExclusive => CLEAR_EXCLUSIVE,
            Self::ArmOp(op) => ARM_OP_BASE + op,
            Self::ResetStacks => RESET_STACKS,
            Self::RedirectOutput => REDIRECT_OUTPUT,
            Self::Variable(entry) => VARIABLE_BASE + entry,
            Self::KeyExit(exit) => KEY_EXIT_BASE + exit,
            Self::Vdu(place) => VDU_BASE + place,
            Self::Builtin(entry) => BUILTIN_BASE + entry,
        }
    }
}

impl Os<'_> {
    /// Performs a kernel routine the processor reached on the host pages,
    /// entered as its address is: acting for code in the processor's mode,
    /// its aborts reported at its own address.
    pub(crate) fn routine(&mut self, routine: Routine) -> Result<(), Unwind> {
        let cpu = self.world.processor.cpu();
        let (user, at) = (!cpu.mode().privileged(), cpu.pc().wrapping_sub(4));
        let user_outer = core::mem::replace(&mut self.world.user_caller, user);
        let at_outer = core::mem::replace(&mut self.world.swi_at, at);
        let result = self.routine_entered(routine);
        self.world.user_caller = user_outer;
        self.world.swi_at = at_outer;
        result
    }

    fn routine_entered(&mut self, routine: Routine) -> Result<(), Unwind> {
        match routine {
            Routine::Return | Routine::PassOn | Routine::Claimed => {
                self.undefined_here();
                Ok(())
            }
            Routine::Exception(slot) => self.exception(slot),
            Routine::Handler(handler) => self.default_handler(handler),
            Routine::Vector(vector) => self.vector_default_entered(vector),
            Routine::HeapArea => self.heap_area_entered(),
            Routine::RamDiscArea => self.ram_disc_area_entered(),
            Routine::ClearExclusive => {
                self.world.processor.cpu().clear_exclusive();
                self.return_to_lr();
                Ok(())
            }
            Routine::ArmOp(op) => {
                if op == CACHE_RANGE_THRESHOLD {
                    let cpu = self.world.processor.cpu();
                    let threshold = cpu.cache(0).map_or(0, |cache| cache.size);
                    cpu.set_reg(Reg::field(0), threshold);
                }
                self.return_to_lr();
                Ok(())
            }
            Routine::ResetStacks => self.reset_stacks(),
            Routine::RedirectOutput => self.redirect_output(),
            Routine::Variable(entry) => self.preset_entered(entry),
            Routine::KeyExit(0) => self.mouse_exit(),
            Routine::KeyExit(_) => self.break_exit(),
            Routine::Vdu(place) => self.vdu_routine_entered(place),
            Routine::Builtin(entry) => self.builtin_entered(entry),
        }
    }
}

/// A host page's `UDF #number`.
fn udf(number: u32) -> Result<u32, MemoryError> {
    instruction(Op::Call {
        op: CallOp::Udf,
        value: number,
    })
}

/// A code variable's code, which works wherever it is copied to: its write
/// entry and then its read, each `LDR PC, [PC, #0]` loading the address it
/// jumps to from the words after them.
pub(crate) fn variable_code(write: u32, read: u32) -> Result<[u32; 4], MemoryError> {
    let entry = jump_through(8)?;
    Ok([entry, entry, write, read])
}

/// `LDR PC, [PC, #-4]`: a jump to the address in the word after it, which
/// works wherever the two words are copied to.
pub(crate) fn jump() -> Result<u32, MemoryError> {
    jump_through(4)
}

/// A processor vector in zero page: it loads its entry from the processor
/// vector table's word at the same slot, which lies the same way on from
/// every vector.
fn vector() -> Result<u32, MemoryError> {
    let ahead = zeropage::PROCESSOR_VECTOR_TABLE - zeropage::VECTORS;
    jump_through(i32::try_from(ahead).map_err(|_| MemoryError::HostFull)?)
}

/// Puts the kernel's processor vector back at `slot`, as when FIQ returns
/// to the kernel and the vector its owner wrote is no longer wanted.
pub(crate) fn restore_vector(memory: &mut Memory, slot: u32) -> Result<(), MemoryError> {
    let word = vector()?;
    let zero = memory.zero_page()[0];
    let page = memory.frame_words_mut(zero).ok_or(MemoryError::NoMemory)?;
    page[slot as usize] = word;
    Ok(())
}

/// Installs the kernel's routines on the host pages, with `builtin_entries`
/// more for the built-in modules' entries, and points zero page's processor
/// vectors at their entries; the routines' base.
pub(crate) fn install(memory: &mut Memory, builtin_entries: u32) -> Result<u32, MemoryError> {
    let routines = BUILTIN_BASE
        .checked_add(builtin_entries)
        .ok_or(MemoryError::HostFull)?;
    let mut block = Vec::new();
    block.try_reserve_exact(routines as usize)?;
    for number in 0..routines {
        block.push(udf(number)?);
    }
    let base = memory.install_host(&block)?;
    let zero = memory.zero_page()[0];
    let table = zeropage::PROCESSOR_VECTOR_TABLE;
    let mut words = [0; EXCEPTIONS.len() * 2];
    words[..EXCEPTIONS.len()].fill(vector()?);
    for (slot, word) in (0..).zip(words.iter_mut().skip(EXCEPTIONS.len())) {
        *word = base + 4 * Routine::Exception(slot).number();
    }
    let page = memory.frame_words_mut(zero).ok_or(MemoryError::NoMemory)?;
    page[..EXCEPTIONS.len()].copy_from_slice(&words[..EXCEPTIONS.len()]);
    let at = (table / 4) as usize;
    page[at..at + EXCEPTIONS.len()].copy_from_slice(&words[EXCEPTIONS.len()..]);
    memory.mirror_vectors(&words[..EXCEPTIONS.len()]);
    Ok(base)
}

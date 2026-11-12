//! Each backend's machine model, behind one entry: a function compiled for a
//! target and run on that target's model instead of its silicon, so a
//! backend can be held to the interpreter on any host.

use alloc::vec;
use alloc::vec::Vec;

use crate::interp::{Memory, Trap};
use crate::ir::{Function, Width};
use crate::lower::Label;
use crate::{verify, CompileError, Isa, Target};

/// Where a model's stack ends, its size, and where it begins.
pub(crate) const STACK_TOP: u64 = 0x7FFF_0000;
const STACK_BYTES: usize = 64 * 1024;
const STACK_BASE: u64 = STACK_TOP - STACK_BYTES as u64;
/// What a callee leaves below the stack pointer.
const SCRIBBLE: u8 = 0xCC;
/// The return address a model enters the function with.
pub(crate) const RETURN: u64 = 0x5EED_0000_0000;
/// What a callee-saved register holds at entry: this, plus its place.
pub(crate) const SAVED_SEED: u64 = 0x5A5A_0000_0000_0000;
/// What a register the code never set reads as, where a partial write makes
/// it defined.
pub(crate) const GARBAGE: u64 = 0xBAD0_BAD0_BAD0_BAD0;

/// Why a run of a model failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The function did not compile.
    Compile(CompileError),
    /// A register read before anything defined it.
    Undefined(u8),
    /// Flags read where no instruction left them defined.
    Flags,
    /// A division the processor refuses: by zero, or a quotient too large.
    Divide,
    /// Memory refused an access.
    Memory(Trap),
    /// The steps allowed ran out.
    Fuel,
    /// A jump to a label never bound, or off the end of the code.
    Unbound,
    /// The stack pointer misaligned at a call.
    Misaligned,
    /// No frame record, linking the caller's, where a call was made.
    Frame,
    /// A register the convention preserves, or the stack pointer, not as it
    /// was at entry when the function returned.
    Clobbered(u8),
    /// A call to an address no helper is at.
    Call(u64),
    /// A helper's argument, by its place, in a register never defined.
    Argument(usize),
    /// A write to a register the platform reserves.
    Reserved(u8),
}

/// What a model reads and writes: its own stack, and the memory beyond it.
pub(crate) struct Space<'a> {
    stack: Vec<u8>,
    memory: &'a mut dyn Memory,
}

impl<'a> Space<'a> {
    pub(crate) fn new(memory: &'a mut dyn Memory) -> Self {
        Self {
            stack: vec![0; STACK_BYTES],
            memory,
        }
    }

    fn stack_bytes(&mut self, addr: u64, n: usize) -> Option<&mut [u8]> {
        let at = usize::try_from(addr.checked_sub(STACK_BASE)?).ok()?;
        self.stack.get_mut(at..at.checked_add(n)?)
    }

    /// The `width` bytes at `addr`, zero-extended.
    pub(crate) fn load(&mut self, addr: u64, width: Width) -> Result<u64, Fault> {
        let n = width.bytes() as usize;
        if let Some(held) = self.stack_bytes(addr, n) {
            let mut raw = [0u8; 8];
            raw[..n].copy_from_slice(held);
            return Ok(u64::from_le_bytes(raw));
        }
        self.memory.load(addr, width).map_err(Fault::Memory)
    }

    /// Store the low `width` bytes of `value` at `addr`.
    pub(crate) fn store(&mut self, addr: u64, width: Width, value: u64) -> Result<(), Fault> {
        let n = width.bytes() as usize;
        if let Some(held) = self.stack_bytes(addr, n) {
            held.copy_from_slice(&value.to_le_bytes()[..n]);
            return Ok(());
        }
        self.memory.store(addr, width, value).map_err(Fault::Memory)
    }

    /// What a callee does to the stack below `sp`: leave nothing there the
    /// caller may read back.
    pub(crate) fn scribble_below(&mut self, sp: u64) {
        let below = sp
            .checked_sub(STACK_BASE)
            .and_then(|below| usize::try_from(below).ok());
        if let Some(bytes) = below.and_then(|below| self.stack.get_mut(..below)) {
            bytes.fill(SCRIBBLE);
        }
    }
}

/// Where each label of a model's instructions is bound.
pub(crate) struct Places(Vec<usize>);

impl Places {
    /// The places of the labels `binds` finds in `insts`.
    pub(crate) fn new<I>(insts: &[I], binds: impl Fn(&I) -> Option<Label>) -> Self {
        let mut places = Vec::new();
        for (n, inst) in insts.iter().enumerate() {
            if let Some(label) = binds(inst) {
                if places.len() <= label.0 {
                    places.resize(label.0 + 1, usize::MAX);
                }
                places[label.0] = n;
            }
        }
        Self(places)
    }

    /// Where `label` is bound.
    pub(crate) fn of(&self, label: Label) -> Result<usize, Fault> {
        self.0
            .get(label.0)
            .copied()
            .filter(|&n| n != usize::MAX)
            .ok_or(Fault::Unbound)
    }
}

/// The helpers a run calls, by the address the code called.
pub trait Calls {
    /// Call the helper at `target` with the target's six argument
    /// registers, those never defined `None`.
    ///
    /// # Errors
    ///
    /// [`Fault::Call`] where no helper is.
    fn call(&mut self, target: u64, args: [Option<u64>; 6]) -> Result<u64, Fault>;
}

/// `func` compiled for `target`, then run on its model with `args`, at most
/// `fuel` instructions: what it returns, if it returns a value.
///
/// # Errors
///
/// The fault that stopped it.
pub fn run(
    target: Target,
    func: &Function,
    args: &[u64],
    memory: &mut dyn Memory,
    calls: &mut dyn Calls,
    fuel: u64,
) -> Result<Option<u64>, Fault> {
    verify::verify(func).map_err(|e| Fault::Compile(CompileError::Verify(e)))?;
    let answers = func.ret().is_some();
    match target.isa() {
        // The encoding must succeed for the model's run to stand for the
        // code's.
        Isa::X86_64 => {
            let (lowered, _) = crate::x86_64::build(func, target).map_err(Fault::Compile)?;
            crate::x86_64::sim::run(&lowered.insts, args, memory, calls, fuel, answers)
        }
        Isa::Aarch64 => {
            let (lowered, _) = crate::aarch64::build(func).map_err(Fault::Compile)?;
            crate::aarch64::sim::run(&lowered.insts, args, memory, calls, fuel, answers)
        }
        Isa::Riscv64 => {
            let (lowered, _) = crate::riscv64::build(func, target).map_err(Fault::Compile)?;
            crate::riscv64::sim::run(&lowered.insts, args, memory, calls, fuel, answers)
        }
    }
}

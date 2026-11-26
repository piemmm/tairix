//! Exceptions (`plans/RISCOS-EMULATOR.md` §2.3, §4.5): the processor
//! vectors in zero page, which `OS_ClaimProcessorVector` lets guest code
//! claim; the kernel's entries they lead to; and the abort traps of
//! `OS_AbortTrap` and abortable areas, which answer for memory the page
//! tables refuse.
//!
//! A translation or permission fault is offered to the traps first. The
//! aborted instruction is run once on a bus that records the accesses the
//! page tables refuse, making the rest itself; each refused access is asked
//! of every trap whose range holds it until one complies; and the
//! instruction runs again with what the traps supplied. An exclusive
//! transfer, which cannot be made of plain ones, and an instruction fetch
//! instead ask a trap to map the memory, and the instruction is run again.

use alloc::vec::Vec;
use core::ops::Range;

use tairix_riscos_arm::bus::{Abort, Bus, Fetch, Width};
use tairix_riscos_arm::cpu::{Exception, State};
use tairix_riscos_arm::mmu::{self, to_page_end, Rights};
use tairix_riscos_arm::psr::{self, Mode};
use tairix_riscos_arm::reg::Reg;

use crate::env::{DATA_ABORT, PREFETCH_ABORT, UNDEFINED};
use crate::error::{exception, Error};
use crate::host::{Routine, EXCEPTIONS};
use crate::memory::layout::HIGH_ZERO_PAGE;
use crate::memory::{rights_bits, Bits, Memory};
use crate::os::{Ended, Entry, Fail, Os, Regs, Target, Transfer, Unwind};
use crate::vectors::{BAD_CLAIM_NUMBER, NAFF_RELEASE};
use crate::zeropage;

/// "Branch through zero".
const BRANCH_THROUGH_ZERO: Error = Error {
    number: exception::BRANCH_THROUGH_ZERO,
    token: "BranchThroughZero",
};

/// The processor vectors a program may claim: all but FIQ's.
const CLAIMABLE: u32 = 7;
/// `OS_ClaimProcessorVector`'s R0 bit claiming the vector, not releasing it.
pub(crate) const CLAIM: u32 = 1 << 8;
/// An abort trap's reasons.
const STORE: u32 = 0;
const LOAD: u32 = 1;
const MEMMAP: u32 = 2;
/// The access was a privileged mode's.
const PRIVILEGED: u32 = 1 << 4;
/// An abortable area's handler reason for an abort.
const AREA_ABORT: u32 = 5;
/// The longest access one instruction makes: sixteen words, or two of the
/// largest floating-point registers more.
const LONGEST_ACCESS: u32 = 128;

/// A registered abort trap.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Trap {
    range: Range<u32>,
    handler: u32,
    workspace: u32,
    bits: Bits,
}

/// A part of one access the page tables refuse: never more than a word,
/// never across a page.
#[derive(Clone, Copy, Debug)]
struct Access {
    address: u32,
    length: u32,
    write: bool,
    /// What a store gives, from its first byte.
    bytes: [u8; 4],
}

/// The bus an aborted instruction is replayed on: each part of an access the
/// page tables refuse is recorded, a load's answered from `loaded`; every
/// other access is the address space's own.
struct ReplayBus<'a> {
    memory: &'a mut Memory,
    accesses: Vec<Access>,
    /// What the traps supplied, by address, for loads.
    loaded: Vec<(u32, u8)>,
    /// Whether accesses the page tables allow are made: not in the dry run.
    commit: bool,
}

/// An access's parts either side of a page boundary: at most two.
fn parts(address: u32, length: u32) -> [(u32, u32); 2] {
    let first = length.min(to_page_end(address));
    [
        (address, first),
        (address.wrapping_add(first), length - first),
    ]
}

impl ReplayBus<'_> {
    fn record(&mut self, access: Access) {
        if self.accesses.len() < LONGEST_ACCESS as usize && self.accesses.try_reserve(1).is_ok() {
            self.accesses.push(access);
        }
    }

    fn supplied(&self, address: u32) -> u8 {
        self.loaded
            .iter()
            .find(|&&(at, _)| at == address)
            .map_or(0, |&(_, byte)| byte)
    }
}

impl Bus for ReplayBus<'_> {
    fn fetch(&mut self, address: u32, privileged: bool) -> Result<Fetch, Abort> {
        self.memory.bus().fetch(address, privileged)
    }

    fn read(&mut self, address: u32, width: Width, privileged: bool) -> Result<u32, Abort> {
        let whole = !self.memory.refuses(address, false, privileged)
            && !self
                .memory
                .refuses(address.wrapping_add(width.bytes() - 1), false, privileged);
        if whole {
            return self.memory.bus().read(address, width, privileged);
        }
        let mut value = 0;
        for (start, length) in parts(address, width.bytes()) {
            if length == 0 {
                continue;
            }
            let refused = self.memory.refuses(start, false, privileged);
            if refused {
                self.record(Access {
                    address: start,
                    length,
                    write: false,
                    bytes: [0; 4],
                });
            }
            for i in 0..length {
                let at = start.wrapping_add(i);
                let byte = if refused {
                    self.supplied(at)
                } else {
                    self.memory
                        .bus()
                        .read(at, Width::Byte, privileged)?
                        .to_le_bytes()[0]
                };
                value |= u32::from(byte) << (8 * at.wrapping_sub(address));
            }
        }
        Ok(value)
    }

    fn write(
        &mut self,
        address: u32,
        width: Width,
        value: u32,
        privileged: bool,
    ) -> Result<(), Abort> {
        let bytes = value.to_le_bytes();
        for (start, length) in parts(address, width.bytes()) {
            if length == 0 {
                continue;
            }
            let from = start.wrapping_sub(address) as usize;
            if self.memory.refuses(start, true, privileged) {
                let mut part = [0; 4];
                part[..length as usize].copy_from_slice(&bytes[from..from + length as usize]);
                self.record(Access {
                    address: start,
                    length,
                    write: true,
                    bytes: part,
                });
            } else if self.commit {
                if length == width.bytes() {
                    self.memory.bus().write(address, width, value, privileged)?;
                } else {
                    for (i, &byte) in (0..).zip(&bytes[from..from + length as usize]) {
                        self.memory.bus().write(
                            start.wrapping_add(i),
                            Width::Byte,
                            u32::from(byte),
                            privileged,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Who answers an abort in a range: a registered trap, or an abortable
/// area's handler over the range the area reserves.
#[derive(Clone, Debug)]
enum Answerer {
    Trap(Trap),
    Area {
        handler: crate::memory::Handler,
        range: Range<u32>,
        /// Where the area's allocated memory ends.
        allocated: u32,
    },
}

impl Answerer {
    /// The range it answers for.
    fn range(&self) -> Range<u32> {
        match self {
            Self::Trap(trap) => trap.range.clone(),
            Self::Area { range, .. } => range.clone(),
        }
    }
}

impl Os<'_> {
    /// The address a processor vector's table entry holds.
    const fn vector_entry(vector: u32) -> u32 {
        HIGH_ZERO_PAGE + zeropage::PROCESSOR_VECTOR_TABLE + 4 * vector
    }

    /// Whether guest code has claimed the SWI vector, so a `SWI` is taken
    /// as the exception for it to see first.
    pub(crate) fn swi_vector_claimed(&mut self) -> bool {
        let ours = self.world.routine_address(Routine::Exception(2));
        self.peek_word(Self::vector_entry(2))
            .is_some_and(|entry| entry != ours)
    }

    /// `OS_ClaimProcessorVector`: R0's vector claimed with R1's handler, or
    /// released back to R1 if it holds R2.
    pub(crate) fn claim_processor_vector(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let vector = regs.r[0] & 0xFF;
        if vector >= CLAIMABLE {
            return Err(self.error(BAD_CLAIM_NUMBER));
        }
        let entry = Self::vector_entry(vector);
        let current = self.read_word(entry)?;
        if regs.r[0] & CLAIM != 0 {
            self.write_word(entry, regs.r[1])?;
            regs.r[1] = current;
        } else {
            if current != regs.r[2] {
                return Err(self.error(NAFF_RELEASE));
            }
            self.write_word(entry, regs.r[1])?;
        }
        Ok(())
    }

    /// `OS_AbortTrap`: a trap over R1 to R2 registered or removed.
    pub(crate) fn abort_trap(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let trap = Trap {
            range: regs.r[1]..regs.r[2],
            handler: regs.r[3],
            workspace: regs.r[4],
            bits: self.caller_bits(),
        };
        match regs.r[0] {
            0 => {
                if self.world.traps.try_reserve(1).is_err() {
                    return Err(self.no_memory());
                }
                self.world.traps.push(trap);
                Ok(())
            }
            1 => {
                let at = self.world.traps.iter().position(|t| {
                    t.range == trap.range
                        && t.handler == trap.handler
                        && t.workspace == trap.workspace
                });
                match at {
                    Some(at) => {
                        self.world.traps.remove(at);
                        Ok(())
                    }
                    None => Err(self.error(crate::error::BAD_PARAMETERS)),
                }
            }
            _ => Err(self.error(crate::error::BAD_PARAMETERS)),
        }
    }

    /// A processor vector's kernel entry, reached through zero page.
    pub(crate) fn exception(&mut self, slot: u32) -> Result<(), Unwind> {
        match EXCEPTIONS.get(slot as usize).copied().flatten() {
            Some(Exception::Irq) => self.interrupt(),
            Some(Exception::Fiq) => self.return_from_exception(4),
            Some(Exception::Undefined) => self.enter_exception_handler(UNDEFINED),
            Some(Exception::PrefetchAbort) => self.prefetch_abort(),
            Some(Exception::DataAbort) => self.data_abort(),
            Some(Exception::Swi) => self.swi_passed_on(),
            Some(Exception::Reset) => self.branch_through_zero(),
            None => self.enter_exception_handler(crate::env::ADDRESS_EXCEPTION),
        }
    }

    /// Returns from an exception whose handler did nothing, to the
    /// instruction `back` bytes before R14.
    fn return_from_exception(&mut self, back: u32) -> Result<(), Unwind> {
        let cpu = self.world.processor.cpu();
        let target = cpu.reg(Reg::LR).wrapping_sub(back);
        let saved = cpu.spsr().ok_or(Unwind::Ended(Ended::Fault))?;
        cpu.set_cpsr(saved)
            .map_err(|_| Unwind::Ended(Ended::Fault))?;
        cpu.set_reg(Reg::PC, target);
        Ok(())
    }

    /// Enters an exception's environment handler as the PRM's pre-veneer
    /// leaves the processor for one: Supervisor mode with interrupts off,
    /// R14 the way back, beside the PSR for a 26-bit handler and with it in
    /// SPSR for a 32-bit one, whose R14 has no room. The kernel's default
    /// is taken here.
    fn enter_exception_handler(&mut self, handler: u32) -> Result<(), Unwind> {
        if self.world.env.is_default(handler) {
            return self.default_handler(handler);
        }
        let slot = self.world.env.slot(handler);
        let cpu = self.world.processor.cpu();
        let back = cpu.reg(Reg::LR);
        let saved = cpu.spsr().ok_or(Unwind::Ended(Ended::Fault))?;
        let mut state = self.world.processor.state();
        let supervisor = psr::Bank::Supervisor;
        let banked = supervisor.index() - psr::Bank::Irq.index();
        let mode = match slot.bits {
            Bits::Bit26 => {
                state.banked[banked][1] = (back & psr::R15_PC) | psr::r15_psr(saved);
                Mode::Supervisor26
            }
            Bits::Bit32 => {
                state.banked[banked][1] = back;
                if let Some(spsr) = supervisor.spsr() {
                    state.spsr[spsr] = saved;
                }
                Mode::Supervisor
            }
        };
        state.cpsr = mode.bits() | psr::I | (saved & psr::NZCV);
        state.pc = slot.address;
        Err(self.transfer(Transfer {
            bits: slot.bits,
            state,
        }))
    }

    /// The SWI vector's entry, reached when guest code that claimed it
    /// passes a `SWI` on: performed as the processor would have stopped at
    /// it, from the mode the exception came from.
    fn swi_passed_on(&mut self) -> Result<(), Unwind> {
        let cpu = self.world.processor.cpu();
        let back = cpu.reg(Reg::LR);
        let saved = cpu.spsr().ok_or(Unwind::Ended(Ended::Fault))?;
        let thumb = saved & psr::T != 0;
        let number = if thumb {
            let half = self.peek_word(back.wrapping_sub(2) & !3).unwrap_or(0);
            (half >> (8 * (back.wrapping_sub(2) & 2))) & 0xFF
        } else {
            self.peek_word(back.wrapping_sub(4)).unwrap_or(0) & 0x00FF_FFFF
        };
        let cpu = self.world.processor.cpu();
        cpu.set_cpsr(saved)
            .map_err(|_| Unwind::Ended(Ended::Fault))?;
        cpu.set_reg(Reg::PC, back);
        self.perform_stopped_swi(number)
    }

    /// A jump to address zero: the registers dumped as they were, and
    /// "branch through zero" raised.
    fn branch_through_zero(&mut self) -> Result<(), Unwind> {
        let state = self.world.processor.state();
        let dump = self.world.env.slot(crate::env::EXCEPTION_DUMP).address;
        for (i, &word) in (0..).zip(&state.user) {
            let _ = self.poke_word(dump + 4 * i, word);
        }
        let _ = self.poke_word(dump + 60, 0);
        let _ = self.poke_word(dump + 64, state.cpsr);
        let block = match self.error(BRANCH_THROUGH_ZERO) {
            Fail::Error(block) => block,
            Fail::Unwind(unwind) => return Err(unwind),
        };
        self.raise(block)
    }

    /// A data abort: an abortable area or a trap answers an access in its
    /// range, from ARM code; any other goes to the data abort handler.
    fn data_abort(&mut self) -> Result<(), Unwind> {
        let (status, address) = self.world.processor.cpu().data_fault();
        let state = self.world.processor.state();
        let saved = self.world.processor.cpu().spsr().unwrap_or(state.cpsr);
        let pc = self.world.processor.cpu().reg(Reg::LR).wrapping_sub(8);
        self.world.last_abort = crate::os::Abort {
            pc,
            psr: saved,
            address,
        };
        if saved & psr::T == 0 && trappable(status) && self.trap_room() {
            match self.replay(state, saved, pc)? {
                Replayed::Done => return Ok(()),
                Replayed::Raise(block) => return self.raise(block),
                Replayed::Declined => {}
            }
        }
        self.enter_exception_handler(DATA_ABORT)
    }

    /// An instruction fetch the page tables refused: a trap holding the
    /// instruction is asked to map it, and it is fetched again.
    fn prefetch_abort(&mut self) -> Result<(), Unwind> {
        let cpu = self.world.processor.cpu();
        let back = cpu.reg(Reg::LR);
        let saved = cpu.spsr().unwrap_or(0);
        // The status, where the processor keeps one: the SA-110 records none.
        let status = cpu.system_register([0, 5, 0, 1]);
        self.world.last_abort = crate::os::Abort {
            pc: back,
            psr: saved,
            address: back,
        };
        let pc = back.wrapping_sub(4);
        if status.is_none_or(trappable) && self.trap_room() && !self.answerers(pc).is_empty() {
            let length = if saved & psr::T == 0 { 4 } else { 2 };
            let privileged = Mode::from_bits(saved).is_some_and(Mode::privileged);
            let wanted = MEMMAP | permissions(privileged, Rights::Read, true);
            match self.ask_all(pc, length, wanted, &[])? {
                Answered::Bytes(_) => return self.return_from_exception(4),
                Answered::Raise(block) => return self.raise(block),
                Answered::Declined => {}
            }
        }
        self.enter_exception_handler(PREFETCH_ABORT)
    }

    /// Whether the supervisor stack has room below its pointer for a
    /// trap's buffer: a pointer outside the stack is not trusted.
    fn trap_room(&mut self) -> bool {
        let sp = self.world.processor.state().banked[1][0];
        let stack = &self.memory.layout().stacks.supervisor;
        sp <= stack.end
            && sp
                .checked_sub(LONGEST_ACCESS)
                .is_some_and(|buffer| buffer >= stack.start)
    }

    /// Every answerer whose range holds `address`: an abortable area's
    /// handler, then the traps in the order they were registered.
    fn answerers(&self, address: u32) -> Vec<Answerer> {
        let mut found = Vec::new();
        if let Some((number, handler)) = self.memory.abortable_at(address) {
            if let Ok(read) = self.memory.read_area(number.cast_signed()) {
                let start = read.start;
                let reserved = start.saturating_add(u32::try_from(read.max).unwrap_or(u32::MAX));
                let allocated = start.saturating_add(u32::try_from(read.size).unwrap_or(u32::MAX));
                if found.try_reserve(1).is_ok() {
                    found.push(Answerer::Area {
                        handler,
                        range: start..reserved,
                        allocated,
                    });
                }
            }
        }
        for trap in self
            .world
            .traps
            .iter()
            .filter(|trap| trap.range.contains(&address))
        {
            if found.try_reserve(1).is_ok() {
                found.push(Answerer::Trap(trap.clone()));
            }
        }
        found
    }

    /// Replays the aborted instruction at `pc`, the traps answering for
    /// what the page tables refuse.
    fn replay(&mut self, aborted: State, saved: u32, pc: u32) -> Result<Replayed, Unwind> {
        let before = State {
            cpsr: saved,
            pc,
            ..aborted
        };
        let bits = self.world.processor.bits();
        if self.world.processor.load(bits, &before).is_err() {
            return Err(Unwind::Ended(Ended::Fault));
        }
        // The dry run: what the page tables would refuse it.
        let refused = self.run_replay(Vec::new(), false);
        if self.world.processor.load(bits, &before).is_err() {
            return Err(Unwind::Ended(Ended::Fault));
        }
        if refused.is_empty() || refused.iter().any(|a| self.answerers(a.address).is_empty()) {
            return self.restore_aborted(aborted, bits);
        }
        let privileged = Mode::from_bits(saved).is_some_and(Mode::privileged);
        if let Some(stores) = self.exclusive_at(pc, saved) {
            for access in &refused {
                let rights = if stores {
                    Rights::ReadWrite
                } else {
                    Rights::Read
                };
                let wanted = MEMMAP | permissions(privileged, rights, false);
                match self.ask_all(access.address, access.length, wanted, &[])? {
                    Answered::Bytes(_) => {}
                    Answered::Declined => return self.restore_aborted(aborted, bits),
                    Answered::Raise(block) => {
                        self.restore_aborted(aborted, bits)?;
                        return Ok(Replayed::Raise(block));
                    }
                }
            }
            // Mapped: the instruction runs again, from the start.
            return Ok(Replayed::Done);
        }
        let mut loaded = Vec::new();
        for access in refused.iter().filter(|a| !a.write) {
            let wanted = LOAD | if privileged { PRIVILEGED } else { 0 };
            match self.ask_all(access.address, access.length, wanted, &[])? {
                Answered::Bytes(bytes) => {
                    for (i, byte) in (0..).zip(bytes) {
                        if loaded.try_reserve(1).is_ok() {
                            loaded.push((access.address.wrapping_add(i), byte));
                        }
                    }
                }
                Answered::Declined => return self.restore_aborted(aborted, bits),
                Answered::Raise(block) => {
                    self.restore_aborted(aborted, bits)?;
                    return Ok(Replayed::Raise(block));
                }
            }
        }
        let accesses = self.run_replay(loaded, true);
        for access in accesses.iter().filter(|a| a.write) {
            let wanted = STORE | if privileged { PRIVILEGED } else { 0 };
            let stored = &access.bytes[..access.length as usize];
            match self.ask_all(access.address, access.length, wanted, stored)? {
                Answered::Bytes(_) => {}
                Answered::Declined => return self.restore_aborted(aborted, bits),
                Answered::Raise(block) => return Ok(Replayed::Raise(block)),
            }
        }
        Ok(Replayed::Done)
    }

    /// Whether the instruction at `pc` is an exclusive transfer, which a
    /// trap must map memory for: then whether it stores.
    fn exclusive_at(&mut self, pc: u32, saved: u32) -> Option<bool> {
        use tairix_riscos_arm::instruction::{ExclusiveOp, Op};
        let privileged = Mode::from_bits(saved).is_some_and(Mode::privileged);
        let word = self.memory.bus().read(pc, Width::Word, privileged).ok()?;
        let isa = self.world.processor.cpu().profile().isa;
        let Op::Exclusive { op, .. } = tairix_riscos_arm::a32::decode(word, isa)?.op else {
            return None;
        };
        match op {
            ExclusiveOp::Ldrex
            | ExclusiveOp::Ldrexb
            | ExclusiveOp::Ldrexh
            | ExclusiveOp::Ldrexd
            | ExclusiveOp::Ldaex
            | ExclusiveOp::Ldaexb
            | ExclusiveOp::Ldaexh
            | ExclusiveOp::Ldaexd => Some(false),
            ExclusiveOp::Strex
            | ExclusiveOp::Strexb
            | ExclusiveOp::Strexh
            | ExclusiveOp::Strexd
            | ExclusiveOp::Stlex
            | ExclusiveOp::Stlexb
            | ExclusiveOp::Stlexh
            | ExclusiveOp::Stlexd => Some(true),
            ExclusiveOp::Lda
            | ExclusiveOp::Ldab
            | ExclusiveOp::Ldah
            | ExclusiveOp::Stl
            | ExclusiveOp::Stlb
            | ExclusiveOp::Stlh => None,
        }
    }

    /// Puts the processor back as the abort left it, for the data abort
    /// handler to have.
    fn restore_aborted(&mut self, aborted: State, bits: Bits) -> Result<Replayed, Unwind> {
        self.world
            .processor
            .load(bits, &aborted)
            .map_err(|_| Unwind::Ended(Ended::Fault))?;
        Ok(Replayed::Declined)
    }

    /// Runs the one instruction on a replay bus; the refused accesses it
    /// made.
    fn run_replay(&mut self, loaded: Vec<(u32, u8)>, commit: bool) -> Vec<Access> {
        let mut bus = ReplayBus {
            memory: self.memory,
            accesses: Vec::new(),
            loaded,
            commit,
        };
        let _ = self.world.processor.cpu().run(&mut bus, 1);
        bus.accesses
    }

    /// Asks the answerers of `length` bytes at `address`, each part of them
    /// of those whose range holds it, until one complies: what a load got,
    /// or the first serious error.
    fn ask_all(
        &mut self,
        address: u32,
        length: u32,
        reason: u32,
        stored: &[u8],
    ) -> Result<Answered, Unwind> {
        let mut got = Vec::new();
        let mut at = 0;
        while at < length {
            let start = address.wrapping_add(at);
            let mut answered = Answered::Declined;
            for answerer in self.answerers(start) {
                let part = (length - at).min(answerer.range().end.wrapping_sub(start));
                let given = stored.get(at as usize..(at + part) as usize).unwrap_or(&[]);
                match self.ask_answerer(&answerer, reason, start, part, given)? {
                    Answered::Bytes(bytes) => {
                        at += part;
                        answered = Answered::Bytes(bytes);
                        break;
                    }
                    Answered::Raise(block) => return Ok(Answered::Raise(block)),
                    Answered::Declined => {}
                }
            }
            match answered {
                Answered::Bytes(bytes) => {
                    if got.try_reserve(bytes.len()).is_ok() {
                        got.extend(bytes);
                    }
                }
                _ => return Ok(Answered::Declined),
            }
        }
        Ok(Answered::Bytes(got))
    }

    /// Asks the trap or area for `length` bytes at `address` (a load), gives
    /// it `stored` (a store), or asks it to map them, through a buffer below
    /// the supervisor stack. An error, or an area's handler doing less than
    /// asked, declines; an error of serious number raises.
    fn ask_answerer(
        &mut self,
        answerer: &Answerer,
        reason: u32,
        address: u32,
        length: u32,
        stored: &[u8],
    ) -> Result<Answered, Unwind> {
        let saved_sp = self.world.processor.state().banked[1][0];
        let buffer = saved_sp.wrapping_sub(LONGEST_ACCESS);
        for (i, &byte) in (0..).zip(stored) {
            let _ = self.poke_byte(buffer.wrapping_add(i), byte);
        }
        let (target, mut regs) = match answerer {
            Answerer::Trap(trap) => (
                Target {
                    address: trap.handler,
                    bits: trap.bits,
                },
                Regs::new([
                    reason,
                    buffer,
                    address,
                    length,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    trap.workspace,
                ]),
            ),
            Answerer::Area {
                handler, allocated, ..
            } => (
                Target {
                    address: handler.code,
                    bits: handler.bits,
                },
                Regs::new([
                    AREA_ABORT,
                    reason,
                    buffer,
                    address,
                    length,
                    *allocated,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    handler.workspace,
                ]),
            ),
        };
        let mut state = self.world.processor.state();
        state.banked[1][0] = buffer;
        let bits = self.world.processor.bits();
        let _ = self.world.processor.load(bits, &state);
        let called = self.call(target, &mut regs, Entry::SupervisorNoIrq);
        let mut state = self.world.processor.state();
        state.banked[1][0] = saved_sp;
        let _ = self.world.processor.load(bits, &state);
        called?;
        if regs.flag(psr::V) {
            let block = regs.r[0];
            let serious = block != 0
                && self
                    .peek_word(block)
                    .is_some_and(|number| number >> 31 != 0);
            return Ok(if serious {
                Answered::Raise(block)
            } else {
                Answered::Declined
            });
        }
        if matches!(answerer, Answerer::Area { .. }) && regs.r[4] != length {
            return Ok(Answered::Declined);
        }
        let mut bytes = Vec::new();
        if reason & 0xF == LOAD {
            for i in 0..length {
                let byte = self.peek_byte(buffer + i).unwrap_or(0);
                if bytes.try_reserve(1).is_ok() {
                    bytes.push(byte);
                }
            }
        }
        Ok(Answered::Bytes(bytes))
    }
}

/// Whether a fault status names a translation or permission fault, the only
/// faults a trap answers for.
const fn trappable(status: u32) -> bool {
    let fault = status & 0xF | (status >> 6) & 0x10;
    matches!(fault, 0b00101 | 0b00111 | 0b01101 | 0b01111)
}

/// A map request's permission bits, from bit 4: the `rights` the mode
/// making the access needs, User or `privileged`, as `OS_Memory` 18 orders
/// them.
fn permissions(privileged: bool, rights: Rights, execute: bool) -> u32 {
    let (user, privileged) = if privileged {
        (Rights::None, rights)
    } else {
        (rights, Rights::None)
    };
    rights_bits(mmu::Access {
        user,
        privileged,
        execute,
    }) << 4
}

/// How a replay ended.
enum Replayed {
    /// The instruction completed.
    Done,
    /// The answerer declined; the data abort handler has it.
    Declined,
    /// The answerer raised this error.
    Raise(u32),
}

/// What an answerer said.
enum Answered {
    /// It took the store, or gave these bytes for the load.
    Bytes(Vec<u8>),
    /// It declined.
    Declined,
    /// It raised this error, at once.
    Raise(u32),
}

impl Os<'_> {
    /// Performs a `SWI` the processor is positioned just after, as a stop
    /// there would.
    fn perform_stopped_swi(&mut self, number: u32) -> Result<(), Unwind> {
        self.world.swi_passing = true;
        let result = self.swi(number);
        self.world.swi_passing = false;
        result
    }
}

#[cfg(test)]
mod tests;

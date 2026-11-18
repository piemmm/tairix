//! Callbacks (`plans/RISCOS-EMULATOR.md` §4.2): the transient list
//! `OS_AddCallBack` builds, the legacy handler `OS_SetCallBack` asks for,
//! and when they run. Transients run on any way back to User mode with
//! interrupts enabled and while the kernel idles waiting for input; the
//! handler only on the way back from the outermost SWI or interrupt, with
//! the supervisor stack empty.

use alloc::vec::Vec;

use tairix_riscos_arm::psr::{self, Mode};
use tairix_riscos_arm::reg::Reg;

use crate::env::CALLBACK;
use crate::memory::Bits;
use crate::os::{Entry, Fail, Os, Regs, Target, Transfer, Unwind};

/// The legacy handler is wanted.
pub(crate) const OLD_STYLE: u8 = 1;
/// A SWI returned an error its caller has yet to read: callbacks wait for
/// the next exit.
const POSTPONE: u8 = 2;
/// The transient list holds callbacks.
const VECTOR_REQUEST: u8 = 4;
/// The zone's offset has changed under automatic daylight saving.
const ZONE_CHANGE: u8 = 8;

/// Where an error block in ROM begins: such a block cannot be overwritten,
/// so an error from it postpones nothing.
const ROM: u32 = 0xFC00_0000;

/// A transient callback: its code, its R12, the profile it runs under and
/// the system heap node its request holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Transient {
    address: u32,
    workspace: u32,
    bits: Bits,
    node: u32,
}

/// The callback state.
pub(crate) struct Callbacks {
    flag: u8,
    /// Oldest first: the head is the last.
    list: Vec<Transient>,
    /// `OS_SetCallBack` returns without looking.
    skip_once: bool,
}

/// A transient request's system heap node: room for its code, its R12 and a
/// link.
const NODE: u32 = 12;

impl Callbacks {
    pub(crate) const fn new() -> Self {
        Self {
            flag: 0,
            list: Vec::new(),
            skip_once: false,
        }
    }

    /// Asks for the legacy handler.
    pub(crate) const fn request_old_style(&mut self) {
        self.flag |= OLD_STYLE;
    }

    /// Asks for the daylight saving changeover the clock found due.
    pub(crate) const fn request_zone_change(&mut self) {
        self.flag |= ZONE_CHANGE;
    }
}

impl Os<'_> {
    /// `OS_AddCallBack`: a callback at the head of the transient list.
    pub(crate) fn add_callback(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        if self.world.callbacks.list.try_reserve(1).is_err() {
            return Err(self.error(crate::error::SYSTEM_HEAP_FULL));
        }
        let node = self.system_node(NODE)?;
        let bits = self.caller_bits();
        self.world.callbacks.list.push(Transient {
            address: regs.r[0],
            workspace: regs.r[1],
            bits,
            node,
        });
        self.world.callbacks.flag |= VECTOR_REQUEST;
        Ok(())
    }

    /// `OS_RemoveCallBack`: every request for the routine and R12 removed.
    #[expect(clippy::unnecessary_wraps, reason = "a SWI handler's signature")]
    pub(crate) fn remove_callback(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let (address, workspace) = (regs.r[0], regs.r[1]);
        while let Some(at) = self
            .world
            .callbacks
            .list
            .iter()
            .position(|t| t.address == address && t.workspace == workspace)
        {
            let removed = self.world.callbacks.list.remove(at);
            self.free_system_node(removed.node);
        }
        Ok(())
    }

    /// `OS_SetCallBack`: the legacy handler asked for, not on this exit.
    #[expect(clippy::unnecessary_wraps, reason = "a SWI handler's signature")]
    pub(crate) fn set_callback(&mut self, _regs: &mut Regs) -> Result<(), Fail> {
        self.world.callbacks.request_old_style();
        self.world.callbacks.skip_once = true;
        Ok(())
    }

    /// Whether the processor is about to return to User mode with
    /// interrupts enabled: where a callback may run.
    fn callback_safe(&mut self) -> bool {
        let cpu = self.world.processor.cpu();
        let user = matches!(cpu.mode(), Mode::User | Mode::User26);
        let waiting = self.world.resume.is_some();
        user && cpu.cpsr() & psr::I == 0 && !waiting
    }

    /// While the kernel idles waiting for input: the transient callbacks,
    /// which may bring the input. Whether any ran.
    pub(crate) fn callbacks_while_idle(&mut self) -> Result<bool, Unwind> {
        if self.world.callbacks.flag & (VECTOR_REQUEST | ZONE_CHANGE) == 0 {
            return Ok(false);
        }
        self.zone_change_due()?;
        if self.world.callbacks.flag & VECTOR_REQUEST != 0 {
            self.run_transients()?;
        }
        Ok(true)
    }

    /// The daylight saving changeover, if the clock found one due.
    fn zone_change_due(&mut self) -> Result<(), Unwind> {
        if self.world.callbacks.flag & ZONE_CHANGE == 0 {
            return Ok(());
        }
        self.world.callbacks.flag &= !ZONE_CHANGE;
        match self.dst_changeover() {
            Ok(()) | Err(crate::os::Fail::Error(_)) => Ok(()),
            Err(crate::os::Fail::Unwind(unwind)) => Err(unwind),
        }
    }

    /// After a SWI from guest code: the callbacks due, if this exit may run
    /// them. An error postpones them once, so its caller can read its block
    /// first.
    pub(crate) fn callbacks_after_swi(&mut self) -> Result<(), Unwind> {
        if core::mem::take(&mut self.world.callbacks.skip_once) {
            return Ok(());
        }
        if self.world.callbacks.flag == 0 || !self.callback_safe() {
            return Ok(());
        }
        let cpu = self.world.processor.cpu();
        if cpu.cpsr() & psr::V != 0 {
            let flag = self.world.callbacks.flag;
            let pending = flag & (OLD_STYLE | VECTOR_REQUEST) != 0;
            let from_rom = cpu.reg(Reg::field(0)) >= ROM;
            if !(flag & POSTPONE != 0 && pending || from_rom) {
                self.world.callbacks.flag |= POSTPONE;
                return Ok(());
            }
        }
        self.run_callbacks()
    }

    /// After an interrupt: the callbacks, where the interrupted code may
    /// take them and no error postpones them.
    pub(crate) fn callbacks_after_interrupt(&mut self) -> Result<(), Unwind> {
        let flag = self.world.callbacks.flag;
        if flag == 0 || flag & POSTPONE != 0 || !self.callback_safe() {
            return Ok(());
        }
        self.run_callbacks()
    }

    /// Runs the transient list, then enters the legacy handler if it was
    /// asked for and the supervisor stack is empty.
    pub(crate) fn run_callbacks(&mut self) -> Result<(), Unwind> {
        self.world.callbacks.flag &= !POSTPONE;
        self.zone_change_due()?;
        if self.world.callbacks.flag & VECTOR_REQUEST != 0 {
            self.run_transients()?;
        }
        if self.world.callbacks.flag & OLD_STYLE != 0 && self.world.depth == 0 {
            self.world.callbacks.flag &= !OLD_STYLE;
            return Err(self.enter_callback_handler());
        }
        Ok(())
    }

    /// The transient list, newest first and those it adds with it.
    fn run_transients(&mut self) -> Result<(), Unwind> {
        self.world.callbacks.flag &= !VECTOR_REQUEST;
        while let Some(callback) = self.world.callbacks.list.pop() {
            self.free_system_node(callback.node);
            let mut regs = Regs::default();
            regs.r[12] = callback.workspace;
            let target = Target {
                address: callback.address,
                bits: callback.bits,
            };
            self.call(target, &mut regs, Entry::SupervisorIrq)?;
        }
        Ok(())
    }

    /// The legacy handler entered: the User registers and where to resume
    /// in its buffer, and it in Supervisor mode with interrupts disabled, R14
    /// pointing at the buffer. A 26-bit handler finds R0 to R15 there, the
    /// PSR in R15; a 32-bit one, whose R15 has no room for it, the CPSR after.
    fn enter_callback_handler(&mut self) -> Unwind {
        let slot = self.world.env.slot(CALLBACK);
        let state = self.world.processor.state();
        let (words, len) = crate::env::dump_words(&state.user, state.pc, state.cpsr, slot.bits);
        for (i, &word) in (0..).zip(&words[..len]) {
            if self
                .poke_word(slot.buffer.wrapping_add(4 * i), word)
                .is_none()
            {
                return Unwind::Ended(crate::os::Ended::Fault);
            }
        }
        let mut entered = self.emptied_stacks(state);
        let mode = match slot.bits {
            Bits::Bit26 => Mode::Supervisor26,
            Bits::Bit32 => Mode::Supervisor,
        };
        entered.cpsr = mode.bits() | psr::I | (state.cpsr & psr::NZCV);
        entered.user[12] = slot.workspace;
        entered.banked[1][1] = slot.buffer;
        entered.pc = slot.address;
        self.transfer(Transfer {
            bits: slot.bits,
            state: entered,
        })
    }
}

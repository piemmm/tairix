//! The debugger on the bus, as an in-circuit emulator sits on one: the
//! breakpoints it holds, what it saw, and where it stops the processor.
//!
//! The board asks it at every boundary where the processor begins an
//! instruction ([`Probe::begins`]), and tells it of every interrupt entered,
//! every port cycle and every memory cycle on a page it watches, whichever
//! master made it. A watch or an interrupt breakpoint stops the processor at
//! the next boundary; a code breakpoint and a step at the boundary itself.
//! Only the processor's own instruction boundaries are observed here, so a
//! translated block, which has none, does not run while anything waits for
//! one ([`Probe::watches_instructions`]).

use tairix_inline::ArrayVec;

use super::disasm::{self, Flow, Reading, Return};
use crate::cpu::{gpr, linear, Bus, Cause, Cpu, CpuModel, Seg};

/// Breakpoints a probe holds at most.
pub const MOST_BREAKPOINTS: usize = 32;
/// The bytes of the address space.
pub const SPACE: u32 = 1 << 20;

/// What a memory watch stops on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// A write.
    Write,
    /// A read or a write; not a code fetch.
    Any,
}

/// What a port watch stops on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// An input.
    In,
    /// An output.
    Out,
    /// Either.
    Both,
}

/// A breakpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Breakpoint {
    /// The processor beginning the instruction at this physical address.
    Code(u32),
    /// A cycle `access` takes on `len` bytes of memory from physical `first`,
    /// within the address space.
    Memory {
        /// The first byte.
        first: u32,
        /// How many, at least one.
        len: u32,
        /// What stops it.
        access: Access,
    },
    /// A cycle `direction` takes on the ports `first..=last`.
    Port {
        /// The first port.
        first: u16,
        /// The last.
        last: u16,
        /// What stops it.
        direction: Direction,
    },
    /// The processor entering interrupt `vector`, AH holding `ah` where one
    /// is given.
    Interrupt {
        /// The vector.
        vector: u8,
        /// The function asked for in AH, if any.
        ah: Option<u8>,
    },
}

impl Breakpoint {
    /// Whether it is one a probe can hold: a memory range within the address
    /// space, a port range in order.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        match *self {
            Self::Code(addr) => addr < SPACE,
            Self::Memory { first, len, .. } => len > 0 && first < SPACE && len <= SPACE - first,
            Self::Port { first, last, .. } => first <= last,
            Self::Interrupt { .. } => true,
        }
    }
}

/// What made a bus cycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Master {
    /// The processor, or the firmware's host calls acting for it.
    Cpu,
    /// The 8087, moving its operand.
    Coprocessor,
    /// A DMA channel.
    Dma,
}

/// The kind of a bus cycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cycle {
    /// A memory read.
    Read,
    /// A memory write.
    Write,
    /// A port input.
    In,
    /// A port output.
    Out,
}

/// A bus cycle a watch saw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    /// What made it.
    pub master: Master,
    /// What it was.
    pub cycle: Cycle,
    /// The physical address, or the port.
    pub at: u32,
    /// The byte it carried.
    pub value: u8,
}

/// Why the processor stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// It was asked to.
    Break,
    /// A step ended.
    Stepped,
    /// A code breakpoint reached, or an interrupt breakpoint's interrupt
    /// entered.
    Breakpoint(Breakpoint),
    /// A memory or port watch saw `hit`, made since the last boundary.
    Watched {
        /// The watch.
        watch: Breakpoint,
        /// What it saw.
        hit: Hit,
    },
}

/// How the processor goes on from a stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Go {
    /// Until something stops it.
    Continue,
    /// Through one instruction, into what it calls, but not into a handler
    /// an external interrupt enters on the way.
    Step,
    /// Through one instruction, and all it calls: a CALL, an INT, a
    /// repeated string instruction.
    StepOver,
    /// Until the procedure the processor is in returns.
    StepOut,
}

/// A stack pointer: the native SS:SP, or in 8080 mode DS:BP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stack {
    segment: u16,
    pointer: u16,
}

impl Stack {
    fn of(cpu: &Cpu) -> Self {
        let regs = cpu.regs();
        if cpu.in_8080_mode() {
            Self {
                segment: regs.sreg(Seg::Ds),
                pointer: regs.gpr[gpr::BP],
            }
        } else {
            Self {
                segment: regs.sreg(Seg::Ss),
                pointer: regs.sp(),
            }
        }
    }

    /// Whether `self` is this frame or one its callers hold.
    fn within(self, frame: Self) -> bool {
        self.segment == frame.segment && self.pointer >= frame.pointer
    }
}

/// What a running processor is stopped at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Goal {
    /// The next boundary.
    Break,
    /// The next instruction begun.
    Step,
    /// Physical `target` reached with the stack at `stack` or above it.
    Reach { target: u32, stack: Stack },
    /// The return of the procedure whose frame is at `stack`.
    Out { stack: Stack },
}

/// The probe.
#[derive(Clone, Debug)]
pub struct Probe {
    model: CpuModel,
    breakpoints: ArrayVec<Breakpoint, MOST_BREAKPOINTS>,
    /// Whether a code breakpoint is held.
    code: bool,
    goal: Option<Goal>,
    /// The boundary a stop was made at, by cycle and address, which the
    /// processor goes on from without stopping there again.
    resume: Option<(u64, u32)>,
    /// A stop due at the next boundary.
    pending: Option<Reason>,
    stopped: Option<Reason>,
}

impl Probe {
    /// A probe for `model`'s processor, holding nothing.
    #[must_use]
    pub const fn new(model: CpuModel) -> Self {
        Self {
            model,
            breakpoints: ArrayVec::new(),
            code: false,
            goal: None,
            resume: None,
            pending: None,
            stopped: None,
        }
    }

    /// The breakpoints it holds, numbered by their place.
    #[must_use]
    pub fn breakpoints(&self) -> &[Breakpoint] {
        &self.breakpoints
    }

    /// Holds `breakpoints` in place of those it held: whether it could, so
    /// many being no more than [`MOST_BREAKPOINTS`], each valid and none
    /// twice. A set it refuses leaves the one it held.
    pub fn set(&mut self, breakpoints: &[Breakpoint]) -> bool {
        let once = |n: usize| !breakpoints[..n].contains(&breakpoints[n]);
        if breakpoints.len() > MOST_BREAKPOINTS
            || !breakpoints.iter().all(Breakpoint::is_valid)
            || !(0..breakpoints.len()).all(once)
        {
            return false;
        }
        self.breakpoints.clear();
        if self.breakpoints.try_extend_from_slice(breakpoints).is_err() {
            return false;
        }
        self.code = breakpoints
            .iter()
            .any(|breakpoint| matches!(breakpoint, Breakpoint::Code(_)));
        true
    }

    /// Holds `breakpoint` too: whether it could, it being valid, not held
    /// already, and the set not full.
    pub fn add(&mut self, breakpoint: Breakpoint) -> bool {
        if !breakpoint.is_valid() || self.breakpoints.contains(&breakpoint) {
            return false;
        }
        let added = self.breakpoints.try_push(breakpoint).is_ok();
        self.code |= added && matches!(breakpoint, Breakpoint::Code(_));
        added
    }

    /// Holds `breakpoint` no more: whether it was held.
    pub fn remove(&mut self, breakpoint: Breakpoint) -> bool {
        let Some(at) = self.breakpoints.iter().position(|held| *held == breakpoint) else {
            return false;
        };
        self.breakpoints.remove(at);
        self.code = self
            .breakpoints
            .iter()
            .any(|held| matches!(held, Breakpoint::Code(_)));
        true
    }

    /// Whether a memory watch covers any byte of `first..=last`.
    #[must_use]
    pub fn watches(&self, first: u32, last: u32) -> bool {
        self.breakpoints.iter().any(|breakpoint| {
            matches!(*breakpoint, Breakpoint::Memory { first: from, len, .. }
                if from <= last && first <= from + (len - 1))
        })
    }

    /// Whether something waits for the processor to begin an instruction:
    /// a code breakpoint, a step, a stop due.
    #[must_use]
    pub const fn watches_instructions(&self) -> bool {
        self.code || self.goal.is_some() || self.pending.is_some()
    }

    /// Whether the processor is stopped.
    #[must_use]
    pub const fn is_stopped(&self) -> bool {
        self.stopped.is_some()
    }

    /// Why the processor stopped, the first time it is asked after a stop.
    pub fn take_stop(&mut self) -> Option<Reason> {
        self.stopped.take()
    }

    /// Stops a running processor at its next boundary, or where it halts.
    pub fn request_break(&mut self) {
        self.goal = Some(Goal::Break);
    }

    /// Sends the processor on from the stop it is at, as `go` says: `cpu`
    /// as it stands there, its code read from `bus`.
    pub fn go<B: Bus + ?Sized>(&mut self, go: Go, cpu: &Cpu, bus: &B) {
        let regs = cpu.regs();
        let (cs, ip) = (regs.sreg(Seg::Cs), regs.ip);
        self.resume = Some((cpu.cycles(), linear(cs, ip)));
        self.stopped = None;
        self.pending = None;
        let stack = Stack::of(cpu);
        self.goal = match go {
            Go::Continue => None,
            Go::Step => Some(Goal::Step),
            Go::StepOver => {
                let (len, flow) = disasm::flow(self.reading(cpu), bus, cs, ip);
                Some(if flow == Flow::Calls && !cpu.halted() {
                    Goal::Reach {
                        target: linear(cs, ip.wrapping_add(len)),
                        stack,
                    }
                } else {
                    Goal::Step
                })
            }
            Go::StepOut => Some(Goal::Out { stack }),
        };
    }

    fn reading(&self, cpu: &Cpu) -> Reading {
        Reading {
            model: self.model,
            emulating: cpu.in_8080_mode(),
        }
    }

    fn stop(&mut self, reason: Reason) -> bool {
        self.goal = None;
        self.stopped = Some(reason);
        true
    }

    /// `cpu` is about to begin an instruction, its code read from `bus`:
    /// whether it stops there.
    pub fn begins<B: Bus + ?Sized>(&mut self, cpu: &Cpu, bus: &B) -> bool {
        let regs = cpu.regs();
        let here = linear(regs.sreg(Seg::Cs), regs.ip);
        if self.resume.take() == Some((cpu.cycles(), here)) {
            self.look_out(cpu, bus);
            return false;
        }
        if let Some(reason) = self.pending.take() {
            return self.stop(reason);
        }
        if self.goal == Some(Goal::Break) {
            return self.stop(Reason::Break);
        }
        // A paused string instruction going on begins nothing.
        if cpu.resumes_string() {
            return false;
        }
        let reached = match self.goal {
            Some(Goal::Step) => true,
            Some(Goal::Reach { target, stack }) => here == target && Stack::of(cpu).within(stack),
            _ => false,
        };
        if reached {
            return self.stop(Reason::Stepped);
        }
        if self.code && self.breakpoints.contains(&Breakpoint::Code(here)) {
            return self.stop(Reason::Breakpoint(Breakpoint::Code(here)));
        }
        self.look_out(cpu, bus);
        false
    }

    /// The processor halted, or waits for TEST: whether it stops there.
    pub fn halted(&mut self) -> bool {
        if let Some(reason) = self.pending.take() {
            return self.stop(reason);
        }
        self.goal == Some(Goal::Break) && self.stop(Reason::Break)
    }

    /// In a step out, the instruction `cpu` is about to begin: where it is
    /// the procedure's return, the step becomes one to where it returns.
    fn look_out<B: Bus + ?Sized>(&mut self, cpu: &Cpu, bus: &B) {
        let Some(Goal::Out { stack: frame }) = self.goal else {
            return;
        };
        let stack = Stack::of(cpu);
        if !stack.within(frame) {
            return;
        }
        let regs = cpu.regs();
        let (_, flow) = disasm::flow(self.reading(cpu), bus, regs.sreg(Seg::Cs), regs.ip);
        let Flow::Returns(how) = flow else {
            return;
        };
        let word = |offset: u16| {
            let at = |n: u16| bus.peek(linear(stack.segment, stack.pointer.wrapping_add(n)));
            u16::from_le_bytes([at(offset), at(offset.wrapping_add(1))])
        };
        let (cs, ip, popped) = match how {
            Return::Near(more) => (regs.sreg(Seg::Cs), word(0), 2u16.wrapping_add(more)),
            Return::Far(more) => (word(2), word(0), 4u16.wrapping_add(more)),
            Return::Interrupt => (word(2), word(0), 6),
            Return::I8080(cc) => {
                if cc.is_some_and(|cc| !cpu.condition80(cc)) {
                    return;
                }
                (regs.sreg(Seg::Cs), word(0), 2)
            }
        };
        let mut after = stack;
        after.pointer = after.pointer.wrapping_add(popped);
        // RETEM leaves 8080 mode: the stack it returns to is the native one.
        if cpu.in_8080_mode() && how == Return::Interrupt {
            after = Stack {
                segment: regs.sreg(Seg::Ss),
                pointer: regs.sp().wrapping_add(6),
            };
        }
        self.goal = Some(Goal::Reach {
            target: linear(cs, ip),
            stack: after,
        });
    }

    /// `cpu` enters interrupt `vector` for `cause`.
    pub fn interrupt(&mut self, vector: u8, cpu: &Cpu, cause: Cause) {
        let regs = cpu.regs();
        if self.pending.is_none() {
            let ah = regs.get8(4);
            self.pending = self
                .breakpoints
                .iter()
                .find(|breakpoint| {
                    matches!(**breakpoint, Breakpoint::Interrupt { vector: v, ah: want }
                        if v == vector && want.is_none_or(|want| want == ah))
                })
                .copied()
                .map(Reason::Breakpoint);
        }
        // A step does not stop in a handler a pin enters: it goes on to
        // where the handler returns.
        if cause == Cause::External && self.goal == Some(Goal::Step) {
            self.goal = Some(Goal::Reach {
                target: linear(regs.sreg(Seg::Cs), regs.ip),
                stack: Stack::of(cpu),
            });
        }
    }

    /// `master` made memory cycle `cycle` carrying `value` at physical
    /// `addr`.
    pub fn memory(&mut self, addr: u32, value: u8, cycle: Cycle, master: Master) {
        if self.pending.is_some() {
            return;
        }
        let found = self
            .breakpoints
            .iter()
            .find(|breakpoint| {
                matches!(**breakpoint, Breakpoint::Memory { first, len, access }
                if addr.wrapping_sub(first) < len
                    && (access == Access::Any || cycle == Cycle::Write))
            })
            .copied();
        self.watch(
            found,
            Hit {
                master,
                cycle,
                at: addr,
                value,
            },
        );
    }

    /// `master` made port cycle `cycle` carrying `value` at `port`.
    pub fn port(&mut self, port: u16, value: u8, cycle: Cycle, master: Master) {
        if self.pending.is_some() {
            return;
        }
        let found = self
            .breakpoints
            .iter()
            .find(|breakpoint| {
                matches!(**breakpoint, Breakpoint::Port { first, last, direction }
                if (first..=last).contains(&port)
                    && match direction {
                        Direction::Both => true,
                        Direction::In => cycle == Cycle::In,
                        Direction::Out => cycle == Cycle::Out,
                    })
            })
            .copied();
        self.watch(
            found,
            Hit {
                master,
                cycle,
                at: u32::from(port),
                value,
            },
        );
    }

    fn watch(&mut self, found: Option<Breakpoint>, hit: Hit) {
        if let Some(watch) = found {
            self.pending = Some(Reason::Watched { watch, hit });
        }
    }
}

#[cfg(test)]
#[path = "probe_tests.rs"]
mod tests;

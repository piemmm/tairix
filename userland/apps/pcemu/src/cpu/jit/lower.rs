//! A block's lowering: the processor's state as values of the code, the
//! bookkeeping every unit does exactly as [`Cpu::step`] does it, and the
//! exits that hand the state back.
//!
//! The block is a straight run of units, so a value made by one unit is
//! defined wherever a later one reads it. A register is loaded from the
//! frame where the code first reads it. A unit stores nothing to the frame
//! until it ends, and then only what it changed: until then the frame holds
//! the machine as it was before the unit, so a check that fails within it,
//! like the cycle limit reached after it, leaves through one return every
//! exit of its kind shares. The status flags are kept as the operation that
//! sets them and computed only where something reads them; the frame
//! carries that operation for [`Cpu::adopt`] to finish. Every access to
//! memory is checked against the page map before the instruction changes
//! anything.
//!
//! [`Cpu::step`]: crate::cpu::Cpu::step
//! [`Cpu::adopt`]: crate::cpu::Cpu::adopt

use alloc::vec::Vec;

use tairix_codegen::ir::{BinOp, Block, Builder, Cond, Function, Type, Value, Width};

use super::emit::{Emit, V};
use super::frame::{self, at};
use super::helper::{self, Kind};
use super::{page, Exit};
use crate::cpu::decode::{Insn, Prefix, Rep};
use crate::cpu::linear;
use crate::cpu::model::CpuModel;
use crate::cpu::regs::{flag, Seg};

const I32: Type = Type::I32;
const I64: Type = Type::I64;

/// Prefixes an instruction may carry and be translated: past these, a run
/// of them is the interpreter's.
pub(crate) const MAX_PREFIXES: usize = 14;

/// The prefixes ahead of an instruction, in order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Prefixes {
    list: [Option<Prefix>; MAX_PREFIXES],
    pub(crate) count: u8,
    /// Where the first is.
    pub(crate) first_ip: u16,
}

impl Prefixes {
    /// Adds `prefix`, at `ip`; false where there is no room for it.
    pub(crate) fn push(&mut self, prefix: Prefix, ip: u16) -> bool {
        let Some(slot) = self.list.get_mut(usize::from(self.count)) else {
            return false;
        };
        if self.count == 0 {
            self.first_ip = ip;
        }
        *slot = Some(prefix);
        self.count += 1;
        true
    }

    /// What is pending after the first `n` have run, as `Cpu::add_prefix`
    /// gathers them.
    pub(crate) fn after(&self, n: u8) -> Pending {
        let mut pending = Pending::default();
        for (k, prefix) in (0..n).zip(self.list.iter().flatten()) {
            if k == 0 {
                pending.first_ip = self.first_ip;
            }
            pending.last_ip = self.first_ip.wrapping_add(u16::from(k));
            pending.count += 1;
            match *prefix {
                Prefix::Segment(seg) => pending.segment = Some(seg),
                Prefix::Rep(rep) => pending.rep = Some(rep),
                Prefix::Lock => pending.lock = true,
            }
        }
        pending
    }

    /// What is pending when the instruction itself runs.
    pub(crate) fn all(&self) -> Pending {
        self.after(self.count)
    }
}

/// The prefixes pending ahead of a unit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Pending {
    pub(crate) segment: Option<Seg>,
    pub(crate) rep: Option<Rep>,
    pub(crate) lock: bool,
    pub(crate) count: u8,
    pub(crate) first_ip: u16,
    pub(crate) last_ip: u16,
}

/// One instruction of the block, as decoded.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Unit {
    pub(crate) insn: Insn,
    /// Where its opcode is.
    pub(crate) ip: u16,
    /// Where the instruction after it begins.
    pub(crate) next: u16,
    /// The prefixes ahead of it, each a unit of its own.
    pub(crate) prefixes: Prefixes,
}

impl Unit {
    /// What is pending when it runs.
    pub(crate) fn pending(&self) -> Pending {
        self.prefixes.all()
    }
}

/// An ALU operation whose status flags are not yet computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Lazy {
    pub(crate) kind: Kind,
    /// The operands, masked to the width: for a logical operation, `a` is
    /// the result and `b` zero.
    pub(crate) a: V,
    pub(crate) b: V,
    /// The carry or borrow in: a 32-bit zero or one.
    pub(crate) carry: V,
    /// The sum or difference before it is masked.
    pub(crate) full: V,
    /// The result.
    pub(crate) r: V,
    pub(crate) word: bool,
}

/// The processor's state as the code holds it. A register, segment
/// register, FLAGS or latch is none until the code first reads or sets it:
/// the frame still holds it as the block was entered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct State {
    gpr: [Option<V>; 8],
    seg: [Option<V>; 4],
    /// Every bit while `status` is none; while it is pending, the bits no
    /// ALU operation sets, and the CF an INC or DEC keeps.
    flags: Option<V>,
    status: Option<Lazy>,
    ind: Option<V>,
    opr: Option<V>,
    /// The cycle count as the unit being lowered began: 64-bit.
    cycles: V,
    fill: V,
    credit: V,
}

/// What an instruction did to the flow of control.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Flow {
    /// On to the next instruction.
    Next,
    /// On to the next instruction, which the block does not hold.
    Fall,
    /// A transfer: the queue empties, its first fetch is charged, and the
    /// block ends at `cs:ip`.
    Jump {
        /// The 32-bit CS, or none where it stays.
        cs: Option<V>,
        /// The 32-bit IP.
        ip: V,
    },
    /// The block ends after this instruction, for the interpreter to take
    /// the next with interrupts inhibited where `inhibit`, a 32-bit value.
    Inhibit(V),
}

/// The frame fields a unit's end stores, as indices into [`FIELDS`].
mod field {
    /// AX to DI.
    pub(super) const GPR: usize = 0;
    /// ES, CS, SS, DS.
    pub(super) const SEG: usize = 8;
    pub(super) const FLAGS: usize = 12;
    pub(super) const IND: usize = 13;
    pub(super) const OPR: usize = 14;
    pub(super) const CYCLES: usize = 15;
    pub(super) const FILL: usize = 16;
    pub(super) const CREDIT: usize = 17;
    pub(super) const STOLEN: usize = 18;
    pub(super) const IP: usize = 19;
    pub(super) const INHIBIT: usize = 20;
    pub(super) const WINDOW_IP: usize = 21;
    pub(super) const WINDOW_BASE: usize = 22;
    /// The segment, the repeat, LOCK, the count, the first and last IP.
    pub(super) const PREFIX: usize = 23;
    /// The code, the carry in, the operands.
    pub(super) const LAZY: usize = 29;
    pub(super) const COUNT: usize = 33;
}

/// Each field's offset and width.
const FIELDS: [(i32, Width); field::COUNT] = [
    (at::GPR, Width::W16),
    (at::GPR + 2, Width::W16),
    (at::GPR + 4, Width::W16),
    (at::GPR + 6, Width::W16),
    (at::GPR + 8, Width::W16),
    (at::GPR + 10, Width::W16),
    (at::GPR + 12, Width::W16),
    (at::GPR + 14, Width::W16),
    (at::SEG, Width::W16),
    (at::SEG + 2, Width::W16),
    (at::SEG + 4, Width::W16),
    (at::SEG + 6, Width::W16),
    (at::FLAGS, Width::W16),
    (at::IND, Width::W16),
    (at::OPR, Width::W16),
    (at::CYCLES, Width::W64),
    (at::FILL, Width::W32),
    (at::CREDIT, Width::W32),
    (at::STOLEN, Width::W32),
    (at::IP, Width::W16),
    (at::INHIBIT, Width::W8),
    (at::WINDOW_IP, Width::W16),
    (at::WINDOW_BASE, Width::W32),
    (at::PREFIX_SEG, Width::W8),
    (at::PREFIX_REP, Width::W8),
    (at::PREFIX_LOCK, Width::W8),
    (at::PREFIX_COUNT, Width::W8),
    (at::PREFIX_FIRST, Width::W16),
    (at::PREFIX_LAST, Width::W16),
    (at::LAZY, Width::W8),
    (at::LAZY_CARRY, Width::W8),
    (at::LAZY_A, Width::W16),
    (at::LAZY_B, Width::W16),
];

/// The frame's fields as the code last stored them or found them there;
/// none for one whose value the code does not hold.
type Image = [Option<V>; field::COUNT];

/// A block's lowering.
pub(crate) struct Lower {
    pub(crate) e: Emit,
    pub(crate) model: CpuModel,
    /// The code segment the block runs in.
    pub(crate) cs: u16,
    frame: Value,
    pages: Value,
    helpers: Value,
    s: State,
    /// Wait states fetches have taken and not yet charged.
    stolen: V,
    /// Bus cycles the current unit's execution has used.
    bus_cycles: V,
    /// The cycles the current unit waited before its execution began, and
    /// those since, or a prefix unit's all: 32-bit, and each a constant
    /// while every cost is.
    stall: V,
    ticks: V,
    /// Where the queue starts after the last instruction set it: the IP
    /// after its bytes, in the code segment.
    window: Option<u16>,
    /// What the frame holds on the path being lowered.
    stored: Image,
    /// Each code page's wait states, by page number.
    code_waits: Vec<(u32, V)>,
    /// The return each kind of exit shares, once made.
    exits: [Option<Block>; 3],
    /// The block being filled.
    open: Block,
}

/// What [`Lower::save`] keeps.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Saved {
    s: State,
    stolen: V,
    bus_cycles: V,
    stall: V,
    ticks: V,
    window: Option<u16>,
    stored: Image,
}

/// An access to memory the page map admitted.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Access {
    /// The host address of its first byte: 64-bit.
    host: Value,
    /// The wait states each of its bytes takes.
    waits: V,
    /// The segment offset.
    offset: V,
}

impl Lower {
    /// The function's entry for the block at `cs:ip`: the frame, the page
    /// map and the helper table as its arguments, the bus interface unit's
    /// state loaded, the code pages' wait states read.
    pub(crate) fn new(model: CpuModel, cs: u16, ip: u16, code_pages: &[u32]) -> Option<Self> {
        let b = Builder::new(&[I64, I64, I64], Some(I64));
        let [frame, pages, helpers] = <[Value; 3]>::try_from(b.params(Block::ENTRY)).ok()?;
        let mut e = Emit::new(b);
        let cycles = e.load64(frame, at::CYCLES);
        let fill = e.load(Width::W32, frame, at::FILL);
        let credit = e.load(Width::W32, frame, at::CREDIT);
        let code_waits = code_pages
            .iter()
            .map(|&page| {
                (
                    page,
                    e.load(Width::W8, pages, page::entry_offset(page) + page::WAITS),
                )
            })
            .collect();
        let first = e.b.block(&[]);
        e.b.jump(first, &[]);
        e.b.switch_to(first);
        let mut stored: Image = [None; field::COUNT];
        let none = V::K(u64::from(frame::NONE));
        for (n, value) in [
            (field::CYCLES, cycles),
            (field::FILL, fill),
            (field::CREDIT, credit),
            (field::STOLEN, V::K(0)),
            (field::IP, V::K(u64::from(ip))),
            (field::INHIBIT, V::K(0)),
            (field::PREFIX, none),
            (field::PREFIX + 1, none),
            (field::PREFIX + 2, V::K(0)),
            (field::PREFIX + 3, V::K(0)),
            (field::PREFIX + 4, V::K(0)),
            (field::PREFIX + 5, V::K(0)),
            (field::LAZY, none),
            (field::LAZY + 1, V::K(0)),
            (field::LAZY + 2, V::K(0)),
            (field::LAZY + 3, V::K(0)),
        ] {
            stored[n] = Some(value);
        }
        Some(Self {
            e,
            model,
            cs,
            frame,
            pages,
            helpers,
            s: State {
                gpr: [None; 8],
                seg: [None; 4],
                flags: None,
                status: None,
                ind: None,
                opr: None,
                cycles,
                fill,
                credit,
            },
            stolen: V::K(0),
            bus_cycles: V::K(0),
            stall: V::K(0),
            ticks: V::K(0),
            window: None,
            stored,
            code_waits,
            exits: [None; 3],
            open: first,
        })
    }

    /// The function, every block ended.
    pub(crate) fn finish(self) -> Result<Function, tairix_codegen::ir::BuildError> {
        self.e.b.finish()
    }

    pub(crate) const fn width(&self) -> u32 {
        if self.model.bus8() {
            1
        } else {
            2
        }
    }

    pub(crate) fn queue_len(&self) -> u32 {
        u32::from(self.model.queue_len())
    }

    /// `n` more cycles: a 32-bit count.
    pub(crate) fn tick(&mut self, n: V) {
        self.ticks = self.e.add(I32, self.ticks, n);
    }

    /// The cycle count now: 64-bit.
    fn now(&mut self) -> V {
        let taken = self.e.add(I32, self.stall, self.ticks);
        let wide = self.e.widen(taken);
        self.e.add(I64, self.s.cycles, wide)
    }

    /// The unit's cycles taken into the count.
    fn settle(&mut self) {
        self.s.cycles = self.now();
        self.stall = V::K(0);
        self.ticks = V::K(0);
    }

    pub(crate) fn tick_k(&mut self, n: u32) {
        self.tick(V::K(u64::from(n)));
    }

    /// The frame's field `n`, which the code has not changed.
    fn fetch(&mut self, n: usize) -> V {
        let (offset, width) = FIELDS[n];
        let value = self.e.load(width, self.frame, offset);
        self.stored[n] = Some(value);
        value
    }

    /// Register `n`: a 16-bit value.
    pub(crate) fn gpr(&mut self, n: usize) -> V {
        if let Some(value) = self.s.gpr[n] {
            return value;
        }
        let value = self.fetch(field::GPR + n);
        self.s.gpr[n] = Some(value);
        value
    }

    /// Register `n` becomes the 16-bit `value`.
    pub(crate) fn set_gpr(&mut self, n: usize, value: V) {
        self.s.gpr[n] = Some(value);
    }

    /// Segment register `seg`; CS the one the block runs in.
    pub(crate) fn seg(&mut self, seg: Seg) -> V {
        let n = seg.index();
        if let Some(value) = self.s.seg[n] {
            return value;
        }
        let value = if seg == Seg::Cs {
            let cs = V::K(u64::from(self.cs));
            self.stored[field::SEG + n] = Some(cs);
            cs
        } else {
            self.fetch(field::SEG + n)
        };
        self.s.seg[n] = Some(value);
        value
    }

    pub(crate) fn set_seg(&mut self, seg: Seg, value: V) {
        self.s.seg[seg.index()] = Some(value);
    }

    /// The 8086's last effective address.
    pub(crate) fn ind(&mut self) -> V {
        if let Some(value) = self.s.ind {
            return value;
        }
        let value = self.fetch(field::IND);
        self.s.ind = Some(value);
        value
    }

    pub(crate) fn set_ind(&mut self, value: V) {
        self.s.ind = Some(value);
    }

    /// The 8086's last operand.
    pub(crate) fn opr(&mut self) -> V {
        if let Some(value) = self.s.opr {
            return value;
        }
        let value = self.fetch(field::OPR);
        self.s.opr = Some(value);
        value
    }

    pub(crate) fn set_opr(&mut self, value: V) {
        self.s.opr = Some(value);
    }

    /// FLAGS as [`State`] holds it.
    fn flags(&mut self) -> V {
        if let Some(value) = self.s.flags {
            return value;
        }
        let value = self.fetch(field::FLAGS);
        self.s.flags = Some(value);
        value
    }

    /// The operation whose status flags are pending.
    pub(crate) const fn status(&self) -> Option<Lazy> {
        self.s.status
    }

    /// The block that returns `why`, which every exit of the kind shares.
    fn exit_block(&mut self, why: Exit) -> Block {
        let slot = why as usize;
        if let Some(block) = self.exits[slot] {
            return block;
        }
        let block = self.e.b.block(&[]);
        self.e.b.switch_to(block);
        let answer = self.e.val(I64, V::K(why as u64));
        self.e.b.ret(Some(answer));
        self.e.b.switch_to(self.open);
        self.exits[slot] = Some(block);
        block
    }

    /// Leave for `target` where the 32-bit `cond` is not zero.
    fn leave_if(&mut self, cond: V, target: Block) {
        let on = self.e.b.block(&[]);
        self.e.branch(cond, target, on);
        self.e.b.switch_to(on);
        self.open = on;
    }

    /// End the path being lowered, returning `why`.
    pub(crate) fn exit(&mut self, why: Exit) {
        let block = self.exit_block(why);
        self.e.b.jump(block, &[]);
    }

    /// Two paths on from here, where the 32-bit `cond` is and is not zero:
    /// the first is lowered on now, the second left to [`Lower::resume`].
    pub(crate) fn fork(&mut self, cond: V) -> Block {
        let (then, other) = (self.e.b.block(&[]), self.e.b.block(&[]));
        self.e.branch(cond, then, other);
        self.e.b.switch_to(then);
        self.open = then;
        other
    }

    /// The lowering's state, to lower a second path from.
    pub(crate) const fn save(&self) -> Saved {
        Saved {
            s: self.s,
            stolen: self.stolen,
            bus_cycles: self.bus_cycles,
            stall: self.stall,
            ticks: self.ticks,
            window: self.window,
            stored: self.stored,
        }
    }

    /// Continue lowering in `block` from `saved`.
    pub(crate) fn resume(&mut self, block: Block, saved: &Saved) {
        self.e.b.switch_to(block);
        self.open = block;
        self.s = saved.s;
        self.stolen = saved.stolen;
        self.bus_cycles = saved.bus_cycles;
        self.stall = saved.stall;
        self.ticks = saved.ticks;
        self.window = saved.window;
        self.stored = saved.stored;
    }

    /// Stores the registers, flags and latches where they changed since
    /// the frame was last stored.
    pub(crate) fn commit_state(&mut self) {
        let mut wanted: Image = [None; field::COUNT];
        let s = self.s;
        for (slot, value) in wanted[field::GPR..].iter_mut().zip(s.gpr) {
            *slot = value;
        }
        for (slot, value) in wanted[field::SEG..].iter_mut().zip(s.seg) {
            *slot = value;
        }
        for (n, value) in [
            (field::FLAGS, s.flags),
            (field::IND, s.ind),
            (field::OPR, s.opr),
        ] {
            wanted[n] = value;
        }
        match s.status {
            Some(lazy) => {
                let code = V::K(u64::from(lazy.kind.code(lazy.word)));
                for (n, value) in [(0, code), (1, lazy.carry), (2, lazy.a), (3, lazy.b)] {
                    wanted[field::LAZY + n] = Some(value);
                }
            }
            None => wanted[field::LAZY] = Some(V::K(u64::from(frame::NONE))),
        }
        self.store_changed(&wanted);
    }

    /// Stores where the unit leaves the processor — IP `ip`, the prefixes
    /// `pending`, interrupts inhibited where `inhibit`, the queue's start —
    /// where it changed.
    fn commit_place(&mut self, ip: V, pending: Pending, inhibit: V) {
        let mut wanted: Image = [None; field::COUNT];
        wanted[field::IP] = Some(ip);
        wanted[field::INHIBIT] = Some(inhibit);
        if let Some(window) = self.window {
            wanted[field::WINDOW_IP] = Some(V::K(u64::from(window)));
            wanted[field::WINDOW_BASE] = Some(V::K(u64::from(self.cs) << 4));
        }
        // With no prefix pending, `Cpu::adopt` reads only the count.
        wanted[field::PREFIX + 3] = Some(V::K(u64::from(pending.count)));
        if pending.count > 0 {
            let (seg, rep) = frame::prefix_codes(pending.segment, pending.rep);
            for (n, value) in [
                (0, u64::from(seg)),
                (1, u64::from(rep)),
                (2, u64::from(pending.lock)),
                (4, u64::from(pending.first_ip)),
                (5, u64::from(pending.last_ip)),
            ] {
                wanted[field::PREFIX + n] = Some(V::K(value));
            }
        }
        self.store_changed(&wanted);
    }

    /// Stores the cycle count and the bus interface unit where they changed.
    fn commit_timing(&mut self) {
        let mut wanted: Image = [None; field::COUNT];
        for (n, value) in [
            (field::CYCLES, self.s.cycles),
            (field::FILL, self.s.fill),
            (field::CREDIT, self.s.credit),
            (field::STOLEN, self.stolen),
        ] {
            wanted[n] = Some(value);
        }
        self.store_changed(&wanted);
    }

    /// Stores each field `wanted` names whose value the frame does not hold.
    fn store_changed(&mut self, wanted: &Image) {
        for ((&(offset, width), &want), have) in FIELDS.iter().zip(wanted).zip(&mut self.stored) {
            if let Some(want) = want.filter(|&want| *have != Some(want)) {
                self.e.store(width, want, self.frame, offset);
                *have = Some(want);
            }
        }
    }

    /// Leave where the cycle limit is reached.
    fn check_limit(&mut self) {
        let limit = self.e.load64(self.frame, at::LIMIT);
        let reached = self.e.cmp(I64, Cond::Uge, self.s.cycles, limit);
        let out = self.exit_block(Exit::Limit);
        self.leave_if(reached, out);
    }

    // The bus interface unit, as `Cpu::await_bytes`, `Cpu::overlap_operands`
    // and `Cpu::prefetch_during` keep it.

    /// Ceiling of `missing` over the fetch width.
    fn fetches_for(&mut self, missing: V) -> V {
        if self.width() == 1 {
            missing
        } else {
            let up = self.e.add(I32, missing, V::K(1));
            self.e.shr(I32, up, 1)
        }
    }

    fn times_width(&mut self, fetches: V) -> V {
        if self.width() == 1 {
            fetches
        } else {
            self.e.shl(I32, fetches, 1)
        }
    }

    /// Takes `needed` bytes from the queue: the clocks spent waiting.
    fn await_bytes(&mut self, needed: u32) -> V {
        let needed = V::K(u64::from(needed));
        let enough = self.e.cmp(I32, Cond::Uge, self.s.fill, needed);
        let missing = self.e.sub(I32, needed, self.s.fill);
        let fetches = self.fetches_for(missing);
        let cost = self.e.shl(I32, fetches, 2);
        let stall = self.e.saturating_sub(I32, cost, self.s.credit);
        let left = self.times_width(fetches);
        let left = self.e.sub(I32, left, missing);
        let drained = self.e.sub(I32, self.s.fill, needed);
        self.s.fill = self.e.select(I32, enough, drained, left);
        self.s.credit = self.e.select(I32, enough, self.s.credit, V::K(0));
        self.e.select(I32, enough, V::K(0), stall)
    }

    /// Takes `operands` trailing bytes fetched during `clocks` of
    /// execution: the clocks execution then waits.
    fn overlap_operands(&mut self, operands: u32, clocks: V) -> V {
        if operands == 0 {
            return V::K(0);
        }
        let operands = V::K(u64::from(operands));
        let enough = self.e.cmp(I32, Cond::Uge, self.s.fill, operands);
        let missing = self.e.sub(I32, operands, self.s.fill);
        let fetches = self.fetches_for(missing);
        let needed = self.e.shl(I32, fetches, 2);
        let busy = self.e.shl(I32, self.bus_cycles, 2);
        let idle = self.e.saturating_sub(I32, clocks, busy);
        let idle = self.e.add(I32, idle, self.s.credit);
        let left = self.times_width(fetches);
        let left = self.e.sub(I32, left, missing);
        let drained = self.e.sub(I32, self.s.fill, operands);
        self.s.fill = self.e.select(I32, enough, drained, left);
        self.s.credit = self.e.select(I32, enough, self.s.credit, V::K(0));
        let more = self.e.add(I32, self.bus_cycles, fetches);
        self.bus_cycles = self.e.select(I32, enough, self.bus_cycles, more);
        let late = self.e.saturating_sub(I32, needed, idle);
        self.e.select(I32, enough, V::K(0), late)
    }

    /// Lets the queue fill during `clocks` of execution. The queue never
    /// holds more than its length, so a full one has no room, fetches
    /// nothing and is left topped, as `Cpu::prefetch_during` leaves it.
    fn prefetch_during(&mut self, clocks: V) {
        let len = V::K(u64::from(self.queue_len()));
        let busy = self.e.shl(I32, self.bus_cycles, 2);
        self.bus_cycles = V::K(0);
        let idle = self.e.saturating_sub(I32, clocks, busy);
        let idle = self.e.add(I32, idle, self.s.credit);
        let room = self.e.sub(I32, len, self.s.fill);
        let can = self.e.shr(I32, idle, 2);
        let wanted = self.fetches_for(room);
        let fetches = self.e.min(I32, can, wanted);
        let bytes = self.times_width(fetches);
        let filled = self.e.add(I32, self.s.fill, bytes);
        // Whole words can overshoot an odd room by a byte.
        let filled = if self.width() == 1 {
            filled
        } else {
            self.e.min(I32, filled, len)
        };
        let topped = self.e.cmp(I32, Cond::Uge, filled, len);
        let spent = self.e.shl(I32, fetches, 2);
        let rest = self.e.sub(I32, idle, spent);
        self.s.credit = self.e.select(I32, topped, V::K(0), rest);
        self.s.fill = filled;
    }

    /// The wait states fetching `len` code bytes from `ip` takes: each
    /// run of them on one page charged at once.
    fn charge_fetch(&mut self, ip: u16, len: u8) {
        let mut run: Option<(u32, u64)> = None;
        for i in 0..u16::from(len) {
            let page = linear(self.cs, ip.wrapping_add(i)) >> page::PAGE_BITS;
            match &mut run {
                Some((on, bytes)) if *on == page => *bytes += 1,
                _ => {
                    if let Some(done) = run.replace((page, 1)) {
                        self.charge_page(done);
                    }
                }
            }
        }
        if let Some(done) = run {
            self.charge_page(done);
        }
    }

    /// The wait states of `bytes` code bytes fetched from `page`.
    fn charge_page(&mut self, (page, bytes): (u32, u64)) {
        let waits = self
            .code_waits
            .iter()
            .find(|&&(at, _)| at == page)
            .map_or(V::K(0), |&(_, waits)| waits);
        let charged = self.e.bin(I32, BinOp::Mul, waits, V::K(bytes));
        self.stolen = self.e.add(I32, self.stolen, charged);
    }

    /// A prefix unit at `ip`, as the step that decodes one runs, leaving
    /// `pending`.
    pub(crate) fn prefix(&mut self, ip: u16, pending: Pending) {
        self.charge_fetch(ip, 1);
        let stall = self.await_bytes(1);
        self.tick(stall);
        self.tick_k(2);
        self.prefetch_during(V::K(2));
        self.settle();
        self.commit_place(V::K(u64::from(ip.wrapping_add(1))), pending, V::K(0));
        self.commit_timing();
        self.check_limit();
    }

    /// Leave before the instruction for the interpreter to take it.
    pub(crate) fn refuse(&mut self) {
        self.exit(Exit::Step);
    }

    /// Leave before the instruction where the 32-bit `failed` is not zero.
    pub(crate) fn refuse_if(&mut self, failed: V) {
        if failed == V::K(0) {
            return;
        }
        let out = self.exit_block(Exit::Step);
        self.leave_if(failed, out);
    }

    /// `u`'s execution begins: its bytes fetched and awaited.
    pub(crate) fn begin(&mut self, u: &Unit) {
        self.charge_fetch(u.ip, u.insn.len);
        self.stall = self.await_bytes(u32::from(u.insn.head));
        self.window = Some(u.next);
    }

    /// `u`'s execution ends with `flow`: what it changed stored, its waits
    /// and refresh charged, its trailing operand bytes overlapped, the queue
    /// let fill, the cycle count and the bus interface unit stored.
    pub(crate) fn end(&mut self, u: &Unit, flow: Flow) {
        let next = V::K(u64::from(u.next));
        let (ip, inhibit) = match flow {
            Flow::Next | Flow::Fall => (next, V::K(0)),
            Flow::Jump { cs, ip } => {
                if let Some(cs) = cs {
                    self.set_seg(Seg::Cs, cs);
                }
                (ip, V::K(0))
            }
            Flow::Inhibit(inhibit) => (next, inhibit),
        };
        // Nothing past here leaves before the unit, so what it changed is
        // stored now, not held across the refresh.
        self.commit_state();
        self.commit_place(ip, Pending::default(), inhibit);
        let refresh = self.refresh();
        let waits = self.e.add(I32, self.stolen, refresh);
        self.stolen = V::K(0);
        self.tick(waits);
        let spent = self.ticks;
        let late = self.overlap_operands(u32::from(u.insn.len - u.insn.head), spent);
        self.tick(late);
        let clocks = self.e.add(I32, spent, late);
        self.prefetch_during(clocks);
        if let Flow::Jump { .. } = flow {
            self.s.fill = V::K(u64::from(self.width()));
            self.s.credit = V::K(0);
        }
        self.settle();
        self.commit_timing();
        match flow {
            Flow::Next => self.check_limit(),
            Flow::Fall | Flow::Jump { .. } => self.exit(Exit::Continue),
            Flow::Inhibit(_) => self.exit(Exit::Step),
        }
    }

    /// The refresh cycles due by now, charged through the bus.
    fn refresh(&mut self) -> V {
        let now = self.now();
        let deadline = self.e.load64(self.frame, at::REFRESH);
        let due = self.e.cmp(I64, Cond::Uge, now, deadline);
        let take = self.e.b.block(&[]);
        let skip = self.e.b.block(&[]);
        let join = self.e.b.block(&[I32]);
        self.e.branch(due, take, skip);
        self.e.b.switch_to(take);
        let cycles = self.e.val(I64, now);
        let stolen = self
            .e
            .b
            .call(
                self.helpers,
                helper::REFRESH,
                &[self.frame, cycles],
                Some(I64),
            )
            .map_or(V::K(0), V::R);
        let stolen = self.e.narrow(stolen);
        let stolen = self.e.val(I32, stolen);
        self.e.b.jump(join, &[stolen]);
        self.e.b.switch_to(skip);
        let none = self.e.val(I32, V::K(0));
        self.e.b.jump(join, &[none]);
        self.e.b.switch_to(join);
        self.open = join;
        self.e
            .b
            .params(join)
            .first()
            .map_or(V::K(0), |&stolen| V::R(stolen))
    }

    /// `seg:offset` admitted by the page map for a byte or, where `word`, a
    /// word, for a load or, where `store`, a store too; leaving before the
    /// instruction where it is not.
    pub(crate) fn access(&mut self, seg: V, offset: V, word: bool, store: bool) -> Access {
        let based = self.e.shl(I32, seg, 4);
        let addr = self.e.add(I32, based, offset);
        let addr = self.e.and(I32, addr, V::K(0xF_FFFF));
        let page = self.e.shr(I32, addr, page::PAGE_BITS);
        let page = self.e.widen(page);
        let entry = self.e.shl(I64, page, page::SHIFT);
        let entry = self.e.add(I64, entry, V::R(self.pages));
        let entry = self.e.val(I64, entry);
        let base = self.e.load64(entry, page::BASE);
        let info = self.e.load(Width::W16, entry, page::WAITS);
        let mut failed = self.e.cmp(I64, Cond::Eq, base, V::K(0));
        if store {
            let kind = self.e.shr(I32, info, 8);
            let writable = self.e.and(I32, kind, V::K(u64::from(page::WRITABLE)));
            let barred = self.e.cmp(I32, Cond::Eq, writable, V::K(0));
            failed = self.e.or(I32, failed, barred);
        }
        let last = V::K(u64::from(page::PAGE_BYTES - 1));
        let within = self.e.and(I32, addr, last);
        if word {
            let edge = self.e.cmp(I32, Cond::Eq, within, last);
            let wraps = self.e.cmp(I32, Cond::Eq, offset, V::K(0xFFFF));
            failed = self.e.or(I32, failed, edge);
            failed = self.e.or(I32, failed, wraps);
        }
        self.refuse_if(failed);
        let within = self.e.widen(within);
        let host = self.e.add(I64, base, within);
        let host = self.e.val(I64, host);
        let waits = self.e.and(I32, info, V::K(0xFF));
        Access {
            host,
            waits,
            offset,
        }
    }

    /// The bus cycles and penalty a transfer of a byte or word at `offset`
    /// takes, as `Cpu::bus_transfer` and `Cpu::word_penalty` count them.
    fn transfer(&mut self, word: bool, offset: V, penalty: bool) {
        if !word {
            self.bus_cycles = self.e.add(I32, self.bus_cycles, V::K(1));
            return;
        }
        let split = if self.model.bus8() {
            V::K(1)
        } else {
            self.e.and(I32, offset, V::K(1))
        };
        let cycles = self.e.add(I32, split, V::K(1));
        self.bus_cycles = self.e.add(I32, self.bus_cycles, cycles);
        if penalty {
            let four = self.e.shl(I32, split, 2);
            self.tick(four);
        }
    }

    /// The byte or word at `a`, its waits taken.
    pub(crate) fn read(&mut self, a: Access, word: bool) -> V {
        self.transfer(word, a.offset, word);
        let waits = if word {
            self.e.shl(I32, a.waits, 1)
        } else {
            a.waits
        };
        self.stolen = self.e.add(I32, self.stolen, waits);
        let width = if word { Width::W16 } else { Width::W8 };
        self.e.load(width, a.host, 0)
    }

    /// Stores the byte or word `value` at `a`, its waits taken.
    pub(crate) fn write(&mut self, a: Access, word: bool, value: V) {
        self.transfer(word, a.offset, word);
        let waits = if word {
            self.e.shl(I32, a.waits, 1)
        } else {
            a.waits
        };
        self.stolen = self.e.add(I32, self.stolen, waits);
        let width = if word { Width::W16 } else { Width::W8 };
        self.e.store(width, value, a.host, 0);
    }

    /// Stores `value` at `a`, but charges the transfer only where the
    /// 32-bit `made` is not zero: where it is, `value` is what the byte or
    /// word already holds, and the interpreter makes no store.
    pub(crate) fn write_if(&mut self, a: Access, word: bool, value: V, made: V) {
        let (ticks, stolen, bus) = (self.ticks, self.stolen, self.bus_cycles);
        self.write(a, word, value);
        self.ticks = self.e.select(I32, made, self.ticks, ticks);
        self.stolen = self.e.select(I32, made, self.stolen, stolen);
        self.bus_cycles = self.e.select(I32, made, self.bus_cycles, bus);
    }

    /// FLAGS, every bit: computed through the interpreter's own arithmetic
    /// where an operation left them pending.
    pub(crate) fn word(&mut self) -> V {
        let base = self.flags();
        let Some(lazy) = self.s.status.take() else {
            return base;
        };
        let code = V::K(u64::from(lazy.kind.code(lazy.word)));
        let [flags, ..] = self.call(helper::FLAGS, &[code, lazy.a, lazy.b, lazy.carry, base]);
        self.s.flags = Some(flags);
        flags
    }

    /// FLAGS becomes `value`, every bit of it.
    pub(crate) fn set_word(&mut self, value: V) {
        self.s.status = None;
        self.s.flags = Some(value);
    }

    /// FLAGS with the bits of `mask`, none a status flag, set where `on`
    /// and cleared where not: the operation pending stays so.
    pub(crate) fn set_control(&mut self, mask: u16, on: bool) {
        let flags = self.flags();
        let flags = if on {
            self.e.or(I32, flags, V::K(u64::from(mask)))
        } else {
            self.e.and(I32, flags, V::K(u64::from(!mask)))
        };
        self.s.flags = Some(flags);
    }

    /// Whether the flag `mask` names is set: a 32-bit zero or one.
    pub(crate) fn bit(&mut self, mask: u16) -> V {
        let pending = self.s.status.filter(|lazy| {
            mask & flag::STATUS != 0 && !(mask == flag::CF && lazy.kind.keeps_carry())
        });
        let Some(lazy) = pending else {
            let flags = self.flags();
            let bit = self.e.and(I32, flags, V::K(u64::from(mask)));
            return self.e.cmp(I32, Cond::Ne, bit, V::K(0));
        };
        let bits = if lazy.word { 16 } else { 8 };
        let top = |e: &mut Emit, v: V| {
            let shifted = e.shr(I32, v, bits - 1);
            e.and(I32, shifted, V::K(1))
        };
        let logic = lazy.kind == Kind::Logic;
        let subtracts = matches!(lazy.kind, Kind::Sub | Kind::Dec);
        match mask {
            flag::ZF if lazy.kind == Kind::Sub && lazy.carry == V::K(0) => {
                self.e.cmp(I32, Cond::Eq, lazy.a, lazy.b)
            }
            flag::ZF => self.e.cmp(I32, Cond::Eq, lazy.r, V::K(0)),
            flag::SF => top(&mut self.e, lazy.r),
            flag::CF | flag::OF if logic => V::K(0),
            flag::CF if subtracts => {
                let b = self.e.add(I32, lazy.b, lazy.carry);
                self.e.cmp(I32, Cond::Ult, lazy.a, b)
            }
            flag::CF => {
                let all = V::K(if lazy.word { 0xFFFF } else { 0xFF });
                self.e.cmp(I32, Cond::Ugt, lazy.full, all)
            }
            flag::OF => {
                let x = if subtracts {
                    let ab = self.e.xor(I32, lazy.a, lazy.b);
                    let ar = self.e.xor(I32, lazy.a, lazy.r);
                    self.e.and(I32, ab, ar)
                } else {
                    let ra = self.e.xor(I32, lazy.r, lazy.a);
                    let rb = self.e.xor(I32, lazy.r, lazy.b);
                    self.e.and(I32, ra, rb)
                };
                top(&mut self.e, x)
            }
            flag::PF => {
                let low = self.e.and(I32, lazy.r, V::K(0xFF));
                let p = self.e.shr(I32, low, 4);
                let p = self.e.xor(I32, p, low);
                let q = self.e.shr(I32, p, 2);
                let p = self.e.xor(I32, p, q);
                let q = self.e.shr(I32, p, 1);
                let p = self.e.xor(I32, p, q);
                let odd = self.e.and(I32, p, V::K(1));
                self.e.xor(I32, odd, V::K(1))
            }
            // AF, which only the decimal adjusts read, or more than one flag:
            // from the word.
            _ => {
                let word = self.word();
                let bit = self.e.and(I32, word, V::K(u64::from(mask)));
                self.e.cmp(I32, Cond::Ne, bit, V::K(0))
            }
        }
    }

    /// Whether any flag `mask` names is set: a 32-bit zero or one.
    pub(crate) fn any(&mut self, mask: u16) -> V {
        if self.s.status.is_none() {
            let flags = self.flags();
            let bits = self.e.and(I32, flags, V::K(u64::from(mask)));
            return self.e.cmp(I32, Cond::Ne, bits, V::K(0));
        }
        let mut any = V::K(0);
        for one in (0..16).map(|n| 1u16 << n).filter(|one| mask & one != 0) {
            let bit = self.bit(one);
            any = self.e.or(I32, any, bit);
        }
        any
    }

    /// `a + b + carry`, or `a - b - carry` where `sub`, the carry a 32-bit
    /// zero or one: the result, its flags left pending.
    pub(crate) fn arith(&mut self, sub: bool, (a, b, carry): (V, V, V), word: bool) -> V {
        let mask = V::K(if word { 0xFFFF } else { 0xFF });
        let a = self.e.and(I32, a, mask);
        let b = self.e.and(I32, b, mask);
        let full = if sub {
            let d = self.e.sub(I32, a, b);
            self.e.sub(I32, d, carry)
        } else {
            let s = self.e.add(I32, a, b);
            self.e.add(I32, s, carry)
        };
        let r = self.e.and(I32, full, mask);
        self.s.status = Some(Lazy {
            kind: if sub { Kind::Sub } else { Kind::Add },
            a,
            b,
            carry,
            full,
            r,
            word,
        });
        r
    }

    /// INC, or DEC where `dec`, of `value`: CF kept, the rest pending.
    pub(crate) fn inc_dec(&mut self, value: V, word: bool, dec: bool) -> V {
        let kept = match self.s.status {
            Some(lazy) if !lazy.kind.keeps_carry() => {
                let carry = self.bit(flag::CF);
                let flags = self.flags();
                let rest = self.e.and(I32, flags, V::K(u64::from(!flag::CF)));
                self.e.or(I32, rest, carry)
            }
            _ => self.flags(),
        };
        let r = self.arith(dec, (value, V::K(1), V::K(0)), word);
        self.s.flags = Some(kept);
        if let Some(lazy) = &mut self.s.status {
            lazy.kind = if dec { Kind::Dec } else { Kind::Inc };
        }
        r
    }

    /// A logical operation's `result`, masked: its flags left pending.
    pub(crate) fn logic(&mut self, result: V, word: bool) -> V {
        let r = self
            .e
            .and(I32, result, V::K(if word { 0xFFFF } else { 0xFF }));
        self.s.status = Some(Lazy {
            kind: Kind::Logic,
            a: r,
            b: V::K(0),
            carry: V::K(0),
            full: r,
            r,
            word,
        });
        r
    }

    /// A helper's packed answer, split: its 16-bit fields from the lowest.
    pub(crate) fn call(&mut self, index: u32, args: &[V]) -> [V; 4] {
        let args: Vec<Value> = args.iter().map(|&a| self.e.val(I32, a)).collect();
        let answer = self.e.b.call(self.helpers, index, &args, Some(I64));
        let answer = answer.map_or(V::K(0), V::R);
        let mut fields = [V::K(0); 4];
        for (field, shift) in fields.iter_mut().zip([0u64, 16, 32, 48]) {
            let shifted = self.e.bin(I64, BinOp::Shr, answer, V::K(shift));
            let low = self.e.narrow(shifted);
            *field = self.e.and(I32, low, V::K(0xFFFF));
        }
        fields
    }
}

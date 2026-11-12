//! Register allocation by linear scan (Poletto and Sarkar, "Linear Scan
//! Register Allocation") over the blocks the entry reaches, in reverse
//! postorder. Each needed value has one interval, from its definition to its
//! last read or the end of the last block it is live out of, found by
//! walking back from each read toward the definition (Brandner et al.,
//! "Computing Liveness Sets for SSA-Form Programs"), so the work follows the
//! liveness rather than blocks times values. A value holds one register or
//! one stack slot for all of its interval. One live across a call prefers a
//! register the call preserves; in one the call does not, the call saves it
//! to the save area around itself. With no register free, the interval read
//! least for its length goes to a slot. An interval ending where another
//! begins does not lend it its register, so no instruction's result shares a
//! register with one of its own operands. Slots are given once every
//! register is, each spilled interval over its whole span, and the save area
//! follows them.

use alloc::collections::BinaryHeap;
use alloc::vec;
use alloc::vec::Vec;
use core::cmp::{Ordering, Reverse};

use crate::ir::{Function, Terminator, Value};
use crate::uses::Uses;
use crate::CompileError;

/// Where a value lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Loc {
    /// A register, by the target's number.
    Reg(u8),
    /// A stack slot of eight bytes, by its index in the frame.
    Slot(u32),
}

/// The registers a target allocates, each numbered below 64.
#[derive(Clone, Copy, Debug)]
pub struct RegClass {
    /// Registers a call may change: preferred for a value no call outlives.
    caller_saved: &'static [u8],
    /// Registers a call keeps, which the prologue saves where used.
    callee_saved: &'static [u8],
    callee_mask: u64,
}

impl RegClass {
    /// The class of `caller_saved` and `callee_saved`. Made in a constant,
    /// it fails to build for a register numbered 64 or more, or one in both.
    #[allow(clippy::similar_names)] // The calling conventions' own names for the two.
    pub(crate) const fn new(caller_saved: &'static [u8], callee_saved: &'static [u8]) -> Self {
        assert!(
            mask(caller_saved) & mask(callee_saved) == 0,
            "a register is both caller- and callee-saved"
        );
        Self {
            caller_saved,
            callee_saved,
            callee_mask: mask(callee_saved),
        }
    }
}

const fn mask(regs: &[u8]) -> u64 {
    let mut mask = 0u64;
    let mut n = 0;
    while n < regs.len() {
        mask |= 1 << regs[n];
        n += 1;
    }
    mask
}

/// The allocation of a function.
#[derive(Clone, Debug)]
pub struct Allocation {
    /// The blocks in the order they are laid out: the entry first.
    pub order: Vec<usize>,
    /// Each value's place; `None` for a value nothing needs, or one of a
    /// block the entry does not reach.
    pub locs: Vec<Option<Loc>>,
    /// Stack slots the frame holds, the save area last.
    pub slots: u32,
    /// The callee-saved registers some value holds, which the prologue
    /// saves and the epilogue restores, in ascending order.
    pub saved: Vec<u8>,
    /// Each call's caller-saved registers holding a value live across it,
    /// the calls in the order they are laid out.
    saves: Vec<Vec<u8>>,
    /// The save area's first slot: a call saves its registers there in
    /// order.
    save_base: u32,
}

impl Allocation {
    /// Where `value` lives.
    #[must_use]
    pub fn loc(&self, value: Value) -> Option<Loc> {
        self.locs.get(value.index()).copied().flatten()
    }

    /// What the `call`th call laid out saves around itself: each register
    /// and the slot it is saved to.
    pub(crate) fn saves(&self, call: usize) -> impl Iterator<Item = (u8, u32)> + '_ {
        self.saves
            .get(call)
            .into_iter()
            .flatten()
            .zip(self.save_base..)
            .map(|(&reg, slot)| (reg, slot))
    }
}

/// One value's live range in instruction positions.
#[derive(Clone, Copy, Debug)]
struct Interval {
    value: Value,
    start: usize,
    end: usize,
    /// The reads of it: what a slot would load.
    reads: usize,
    crosses_call: bool,
}

impl Interval {
    /// How `self`'s reads for its length compare with `other`'s: the
    /// lesser is the cheaper to hold in a slot.
    fn density(&self, other: &Self) -> Ordering {
        let wide = |n: usize| u128::from(u64::try_from(n).unwrap_or(u64::MAX));
        let weight = |a: &Self, b: &Self| wide(a.reads + 1) * wide(b.end - b.start + 1);
        weight(self, other).cmp(&weight(other, self))
    }
}

/// Instruction positions: each laid-out block's entry, then one an
/// instruction, then its terminator.
struct Layout {
    /// Each block's entry position; meaningless for one not laid out.
    starts: Vec<usize>,
    /// Each block's terminator position.
    ends: Vec<usize>,
    /// The positions of calls, ascending.
    calls: Vec<usize>,
}

impl Layout {
    fn new(func: &Function, order: &[usize]) -> Self {
        let count = func.blocks().len();
        let mut starts = vec![0; count];
        let mut ends = vec![0; count];
        let mut calls = Vec::new();
        let mut at = 0;
        for &block in order {
            starts[block] = at;
            for (_, inst) in &func.blocks()[block].insts {
                at += 1;
                if inst.is_call() {
                    calls.push(at);
                }
            }
            ends[block] = at + 1;
            at += 2;
        }
        Self {
            starts,
            ends,
            calls,
        }
    }
}

/// Every read of a needed value in a register or slot, with its block and
/// position: kept instructions' reads, branch conditions or the operands of
/// the comparisons branches test directly, returned values, and edge
/// arguments to needed parameters.
fn each_read(
    func: &Function,
    order: &[usize],
    layout: &Layout,
    uses: &Uses,
    mut read: impl FnMut(Value, usize, usize),
) {
    for &block in order {
        let data = &func.blocks()[block];
        let start = layout.starts[block];
        for (n, (defined, inst)) in data.insts.iter().enumerate() {
            if uses.kept(*defined, inst) {
                uses.reads(func, *defined, inst, |value| {
                    read(value, block, start + 1 + n);
                });
            }
        }
        let end = layout.ends[block];
        match data.term {
            Terminator::Branch { cond, .. } => match uses.fused(block) {
                Some(compare) => {
                    read(compare.a, block, end);
                    if uses.branch_folded(func, compare).is_none() {
                        read(compare.b, block, end);
                    }
                }
                None => read(cond, block, end),
            },
            Terminator::Return { value: Some(value) } => read(value, block, end),
            Terminator::Return { value: None } | Terminator::Jump { .. } => {}
        }
        for (to, args) in data.term.successors() {
            for (&param, &arg) in func.blocks()[to.index()].params.iter().zip(args) {
                if uses.live(param) {
                    read(arg, block, end);
                }
            }
        }
    }
}

/// The needed values' intervals, in the order they begin.
fn intervals(func: &Function, order: &[usize], layout: &Layout, uses: &Uses) -> Vec<Interval> {
    let count = func.value_count();
    // Each value's reads, together: `first[v]..first[v + 1]` of `reads`.
    let mut first = vec![0usize; count + 1];
    each_read(func, order, layout, uses, |value, _, _| {
        first[value.index() + 1] += 1;
    });
    for n in 0..count {
        first[n + 1] += first[n];
    }
    let mut next = first.clone();
    let mut reads = vec![(0usize, 0usize); first[count]];
    each_read(func, order, layout, uses, |value, block, at| {
        reads[next[value.index()]] = (block, at);
        next[value.index()] += 1;
    });
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); func.blocks().len()];
    for &block in order {
        for (to, _) in func.blocks()[block].term.successors() {
            preds[to.index()].push(block);
        }
    }
    // The value whose walk last found each block live on entry.
    let mut live_in = vec![usize::MAX; func.blocks().len()];
    let mut walk = Vec::new();
    let mut out = Vec::new();
    for &block in order {
        let data = &func.blocks()[block];
        let start = layout.starts[block];
        let params = data.params.iter().map(|&param| (param, start));
        let results = data
            .insts
            .iter()
            .enumerate()
            .filter_map(|(n, (defined, _))| defined.map(|value| (value, start + 1 + n)));
        for (value, start) in params.chain(results) {
            if !uses.live(value) {
                continue;
            }
            let v = value.index();
            let mut end = start;
            for &(read_block, at) in &reads[first[v]..first[v + 1]] {
                end = end.max(at);
                if read_block != block {
                    walk.push(read_block);
                }
            }
            // Live on entry to each block walked, so live out of each of
            // its predecessors, back to the defining block.
            while let Some(entered) = walk.pop() {
                if live_in[entered] == v {
                    continue;
                }
                live_in[entered] = v;
                for &pred in &preds[entered] {
                    end = end.max(layout.ends[pred]);
                    if pred != block && live_in[pred] != v {
                        walk.push(pred);
                    }
                }
            }
            let call = layout.calls.partition_point(|&call| call <= start);
            let crosses_call = layout.calls.get(call).is_some_and(|&call| call < end);
            out.push(Interval {
                value,
                start,
                end,
                reads: first[v + 1] - first[v],
                crosses_call,
            });
        }
    }
    out
}

/// Allocate the needed values of `func`, its blocks laid out in `order`, to
/// `class`'s registers and stack slots.
///
/// # Errors
///
/// [`CompileError::TooLarge`] for more slots than a frame numbers.
pub(crate) fn allocate(
    func: &Function,
    order: Vec<usize>,
    uses: &Uses,
    class: &RegClass,
) -> Result<Allocation, CompileError> {
    let layout = Layout::new(func, &order);
    let intervals = intervals(func, &order, &layout, uses);
    let mut locs: Vec<Option<Loc>> = vec![None; func.value_count()];
    // Intervals holding registers: each's end, register and index.
    let mut active: Vec<(usize, u8, usize)> = Vec::new();
    let mut held = 0u64;
    let mut ever = 0u64;
    let mut spilled: Vec<usize> = Vec::new();
    for (n, interval) in intervals.iter().enumerate() {
        active.retain(|&(end, reg, _)| {
            // An interval ending where this one begins is still live there.
            let live = end >= interval.start;
            if !live {
                held &= !(1 << reg);
            }
            live
        });
        // A register a call keeps first for a value one outlives, so the
        // call need not save it.
        let preferred = if interval.crosses_call {
            [class.callee_saved, class.caller_saved]
        } else {
            [class.caller_saved, class.callee_saved]
        };
        let free = preferred
            .into_iter()
            .flatten()
            .copied()
            .find(|&reg| held >> reg & 1 == 0);
        // With none free, the interval read least for its length, this one
        // or one holding a register, goes to a slot; of equals, the one
        // ending last.
        let reg = free.or_else(|| {
            let (at, &(_, reg, victim)) = active.iter().enumerate().min_by(|(_, a), (_, b)| {
                let (a, b) = (&intervals[a.2], &intervals[b.2]);
                a.density(b).then(b.end.cmp(&a.end))
            })?;
            if intervals[victim].density(interval) != Ordering::Less {
                return None;
            }
            active.swap_remove(at);
            locs[intervals[victim].value.index()] = None;
            spilled.push(victim);
            Some(reg)
        });
        match reg {
            Some(reg) => {
                held |= 1 << reg;
                ever |= 1 << reg;
                active.push((interval.end, reg, n));
                locs[interval.value.index()] = Some(Loc::Reg(reg));
            }
            None => spilled.push(n),
        }
    }
    let save_base = assign_slots(&intervals, &mut spilled, &mut locs)?;
    let around_calls = saves(&intervals, &locs, &layout, class);
    let area = around_calls.iter().map(Vec::len).max().unwrap_or(0);
    let slots = u32::try_from(area)
        .ok()
        .and_then(|area| save_base.checked_add(area))
        .ok_or(CompileError::TooLarge)?;
    let mut saved: Vec<u8> = class
        .callee_saved
        .iter()
        .copied()
        .filter(|&reg| ever >> reg & 1 == 1)
        .collect();
    saved.sort_unstable();
    Ok(Allocation {
        order,
        locs,
        slots,
        saved,
        saves: around_calls,
        save_base,
    })
}

/// Each call's caller-saved registers holding a value live across it: one
/// defined before the call and read after it.
fn saves(
    intervals: &[Interval],
    locs: &[Option<Loc>],
    layout: &Layout,
    class: &RegClass,
) -> Vec<Vec<u8>> {
    let mut saves = vec![Vec::new(); layout.calls.len()];
    for interval in intervals.iter().filter(|interval| interval.crosses_call) {
        let Some(Some(Loc::Reg(reg))) = locs.get(interval.value.index()) else {
            continue;
        };
        if class.callee_mask >> reg & 1 == 1 {
            continue;
        }
        let first = layout.calls.partition_point(|&call| call <= interval.start);
        let crossed = layout.calls[first..]
            .iter()
            .take_while(|&&call| call < interval.end);
        for (save, _) in saves[first..].iter_mut().zip(crossed) {
            save.push(*reg);
        }
    }
    saves
}

/// Give each spilled interval a slot no other interval holds anywhere in
/// its span: greedily by start, which for intervals uses the fewest slots.
fn assign_slots(
    intervals: &[Interval],
    spilled: &mut [usize],
    locs: &mut [Option<Loc>],
) -> Result<u32, CompileError> {
    spilled.sort_unstable_by_key(|&n| (intervals[n].start, n));
    let mut busy: BinaryHeap<Reverse<(usize, u32)>> = BinaryHeap::new();
    let mut free: Vec<u32> = Vec::new();
    let mut count = 0u32;
    for &n in spilled.iter() {
        let interval = intervals[n];
        while let Some(&Reverse((end, slot))) = busy.peek() {
            if end >= interval.start {
                break;
            }
            busy.pop();
            free.push(slot);
        }
        let slot = if let Some(slot) = free.pop() {
            slot
        } else {
            let slot = count;
            count = count.checked_add(1).ok_or(CompileError::TooLarge)?;
            slot
        };
        busy.push(Reverse((interval.end, slot)));
        locs[interval.value.index()] = Some(Loc::Slot(slot));
    }
    Ok(count)
}

#[cfg(test)]
#[path = "regalloc_tests.rs"]
mod tests;

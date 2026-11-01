//! Registers allocated over live ranges: each virtual register lives from the
//! first position that names it to the last in the laid-out order, widened
//! over the loops other lanes may run again while a lane holds its value — to
//! a loop's end for a value defined before it and used within it, and to its
//! start for a tile-shared value carried out of it. Registers whose ranges do
//! not meet share a slot, as does a whole copy's destination, live from the
//! copy, with a source written before it and last read there; the scratch
//! planes sit above every other.

use alloc::collections::BinaryHeap;
use alloc::vec::Vec;
use core::cmp::Reverse;

use tairix_collections::RadixTree;

use super::layout::{Edge, Laid, Segment, Term};
use super::{
    filled, next_index, push, Fault, Lowering, MAX_PROGRAM_INSTRUCTIONS, SCRATCH, VIRTUAL,
};
use crate::software::shader::ir::{self, Block, Image, Inst, Move, Reg, Step};

/// The most steps the widening of live ranges over loops takes in all.
const MAX_WIDENING_STEPS: usize = 1 << 26;

/// Each register an instruction names, with whether it defines it.
#[expect(clippy::too_many_lines, reason = "one arm per lowered instruction")]
fn visit(
    inst: &mut Inst,
    (steps, images): (&mut [Step], &mut [Image]),
    f: &mut dyn FnMut(&mut Reg, bool),
) {
    match inst {
        Inst::Copy { dst, src, .. } => {
            f(src, false);
            f(dst, true);
        }
        Inst::Zero { dst, .. } => f(dst, true),
        Inst::Map1 { dst, a, .. }
        | Inst::Convert { dst, a, .. }
        | Inst::AnyAll { dst, a, .. }
        | Inst::Pack { dst, a, .. }
        | Inst::Derivative { dst, a, .. } => {
            f(a, false);
            f(dst, true);
        }
        Inst::Map2 { dst, a, b, .. }
        | Inst::Shift { dst, a, b, .. }
        | Inst::Compare { dst, a, b, .. }
        | Inst::Ldexp { dst, a, b, .. }
        | Inst::Group { dst, a, b, .. } => {
            f(a, false);
            f(b, false);
            f(dst, true);
        }
        Inst::Map3 { dst, a, b, c, .. } | Inst::Linear { dst, a, b, c, .. } => {
            f(a, false);
            f(b, false);
            f(c, false);
            f(dst, true);
        }
        Inst::Select {
            dst, cond, a, b, ..
        } => {
            f(cond, false);
            f(a, false);
            f(b, false);
            f(dst, true);
        }
        Inst::Wide {
            dst, second, a, b, ..
        } => {
            f(a, false);
            f(b, false);
            f(dst, true);
            f(second, true);
        }
        Inst::BitField {
            dst,
            base,
            insert,
            offset,
            bits,
            ..
        } => {
            f(base, false);
            f(insert, false);
            f(offset, false);
            f(bits, false);
            f(dst, true);
        }
        Inst::Extract {
            dst, vector, index, ..
        } => {
            f(vector, false);
            f(index, false);
            f(dst, true);
        }
        Inst::Insert {
            dst,
            vector,
            value,
            index,
            ..
        } => {
            f(vector, false);
            f(value, false);
            f(index, false);
            f(dst, true);
        }
        Inst::Chain {
            dst,
            base,
            steps: range,
        } => {
            f(base, false);
            if let Some(steps) = steps.get_mut(range.0 as usize..range.1 as usize) {
                for step in steps {
                    match step {
                        Step::Index { index, .. } | Step::Element { index, .. } => f(index, false),
                        Step::Bytes(_) => {}
                    }
                }
            }
            f(dst, true);
        }
        Inst::Load { dst, ptr, .. } | Inst::ArrayLength { dst, ptr, .. } => {
            f(ptr, false);
            f(dst, true);
        }
        Inst::Store { src, ptr, .. } => {
            f(src, false);
            f(ptr, false);
        }
        Inst::Atomic { ptr, dst, a, b, .. } => {
            f(ptr, false);
            f(a, false);
            f(b, false);
            f(dst, true);
        }
        Inst::TexelPointer {
            dst,
            handle,
            coord,
            sample,
            ..
        } => {
            f(handle, false);
            f(coord, false);
            f(sample, false);
            f(dst, true);
        }
        Inst::Image(at) => {
            if let Some(image) = images.get_mut(*at as usize) {
                visit_image(image, f);
            }
        }
        Inst::Fence(_) => {}
    }
}

fn visit_image(image: &mut Image, f: &mut dyn FnMut(&mut Reg, bool)) {
    f(&mut image.handle, false);
    f(&mut image.coord, false);
    let o = &mut image.operands;
    for reg in [
        &mut o.dref,
        &mut o.bias,
        &mut o.lod,
        &mut o.offset,
        &mut o.offsets,
        &mut o.sample,
    ]
    .into_iter()
    .flatten()
    {
        f(reg, false);
    }
    if let Some((x, y)) = &mut o.grad {
        f(x, false);
        f(y, false);
    }
    match &mut image.op {
        ir::ImageOp::Write { texel, .. }
        | ir::ImageOp::Gather {
            component: Some(texel),
            ..
        } => f(texel, false),
        ir::ImageOp::QuerySizeLod { lod } => f(lod, false),
        _ => {}
    }
    if !matches!(image.op, ir::ImageOp::Write { .. }) {
        f(&mut image.dst, true);
    }
}

/// A copy of the whole of one virtual register into another of its size and
/// class: the one copied into, where the copy lies, and the one copied.
#[derive(Copy, Clone, Debug)]
struct Copied {
    dst: u32,
    at: u32,
    src: u32,
}

/// A register's live range, as positions in the laid-out order.
#[derive(Copy, Clone, Debug)]
struct Range {
    start: u32,
    end: u32,
}

impl Lowering<'_> {
    /// Allocate every virtual register a slot and build the final program
    /// from the laid-out blocks.
    pub(crate) fn allocate(&mut self, mut laid: Laid) -> Result<(), Fault> {
        let starts = positions(&laid)?;
        let (mut ranges, copies) = self.live_ranges(&mut laid, &starts)?;
        widen(&mut ranges, &self.virtuals, &laid, &starts)?;
        let constants = next_index(&self.program.constants)?;
        let assigned = assign(&self.virtuals, constants, &ranges, &copies)?;
        let varying_top = assigned.varying_top;
        let virtuals = &self.virtuals;
        let map = |reg: Reg| physical(virtuals, &assigned, reg);
        let mut steps = core::mem::take(&mut self.program.steps);
        let mut images = core::mem::take(&mut self.program.images);
        let rebuilt = rebuild(laid, (&mut steps, &mut images), &map, assigned.handed)?;
        self.program.steps = steps;
        self.program.images = images;
        self.program.insts = rebuilt.insts;
        self.program.blocks = rebuilt.blocks;
        self.program.edges = rebuilt.edges;
        self.program.moves = rebuilt.moves;
        self.program.cases = rebuilt.cases;
        self.program.scratch = Reg::varying(varying_top);
        self.program.varying_words = varying_top
            .checked_add(self.scratch_words)
            .ok_or(Fault::Budget)?;
        let constants = next_index(&self.program.constants)?;
        self.program.uniform_words = constants
            .checked_add(assigned.uniform_top)
            .ok_or(Fault::Budget)?;
        Ok(())
    }

    /// Each virtual register's first and last position in the laid-out
    /// order, `None` for one no instruction names; and the first whole copy
    /// into each register that has one, by the register copied into.
    fn live_ranges(
        &mut self,
        laid: &mut Laid,
        starts: &[u32],
    ) -> Result<(Vec<Option<Range>>, Vec<Copied>), Fault> {
        let mut ranges: Vec<Option<Range>> = Vec::new();
        ranges
            .try_reserve_exact(self.virtuals.len())
            .map_err(|_| Fault::OutOfMemory)?;
        ranges.resize(self.virtuals.len(), None);
        let mut copies = Vec::new();
        let virtuals = &self.virtuals;
        let mut note = |reg: Reg, at: u32| {
            if let Some(range) =
                virtual_index(virtuals, reg).and_then(|index| ranges.get_mut(index))
            {
                *range = Some(match *range {
                    Some(seen) => Range {
                        start: seen.start.min(at),
                        end: seen.end.max(at),
                    },
                    None => Range { start: at, end: at },
                });
            }
        };
        let (steps, images) = (&mut self.program.steps, &mut self.program.images);
        for (block, &start) in laid.blocks.iter_mut().zip(starts) {
            for (offset, inst) in (0u32..).zip(block.insts.iter_mut()) {
                if let Some((dst, src)) = whole_copy(virtuals, inst) {
                    let found = Copied {
                        dst: u32::try_from(dst).map_err(|_| Fault::Budget)?,
                        at: start + offset,
                        src: u32::try_from(src).map_err(|_| Fault::Budget)?,
                    };
                    push(&mut copies, found, MAX_PROGRAM_INSTRUCTIONS)?;
                }
                visit(inst, (steps, images), &mut |reg, _| {
                    note(*reg, start + offset);
                });
            }
            let at = start + next_index(&block.insts)?;
            visit_term(&mut block.term, &mut |reg, _| note(*reg, at));
        }
        copies.sort_unstable_by_key(|copied| (copied.dst, copied.at));
        copies.dedup_by_key(|copied| copied.dst);
        Ok((ranges, copies))
    }
}

/// Give each virtual register with a live range a slot: the slot of the
/// register it copies whole where its own range begins at that copy and the
/// source's began before it and ends there, so the copy goes; else a slot
/// freed by a range already ended where one of its size is; else a new one.
///
/// A source whose range only starts at the copy was never written and holds
/// no slot to pass on; a destination live before its copy, its range widened
/// over a loop, is live beside its source, so the two cannot share one.
fn assign(
    virtuals: &[(u32, u32, bool)],
    constants: u32,
    ranges: &[Option<Range>],
    copies: &[Copied],
) -> Result<Assigned, Fault> {
    let mut order: Vec<usize> = Vec::new();
    order
        .try_reserve_exact(ranges.len())
        .map_err(|_| Fault::OutOfMemory)?;
    order.extend((0..ranges.len()).filter(|&i| ranges.get(i).is_some_and(Option::is_some)));
    order.sort_unstable_by_key(|&i| ranges.get(i).copied().flatten().map_or(0, |r| r.start));
    let mut slots = Vec::new();
    slots
        .try_reserve_exact(ranges.len())
        .map_err(|_| Fault::OutOfMemory)?;
    slots.resize(ranges.len(), 0u32);
    let mut pools = [Pool::new(0), Pool::new(constants)];
    // Whether a register's slot was handed to the one copying it, which frees
    // it in its stead.
    let mut passed = filled(ranges.len(), false)?;
    let mut handed = 0usize;
    // Ranges still live, the one ending first on top: as many as are live at
    // once, so grown as they come rather than for every range.
    let mut live: BinaryHeap<Reverse<(u32, usize)>> = BinaryHeap::new();
    for index in order {
        let Some(range) = ranges.get(index).copied().flatten() else {
            continue;
        };
        while let Some(&Reverse((end, ended))) = live.peek() {
            if end >= range.start {
                break;
            }
            live.pop();
            if passed.get(ended) == Some(&true) {
                continue;
            }
            let (_, words, uniform) = virtuals.get(ended).copied().ok_or(Fault::Malformed)?;
            let slot = slots.get(ended).copied().unwrap_or(0);
            pools[usize::from(uniform)].free(slot, words)?;
        }
        let (_, words, uniform) = virtuals.get(index).copied().ok_or(Fault::Malformed)?;
        let inherited = copies
            .binary_search_by_key(&index, |copied| copied.dst as usize)
            .ok()
            .and_then(|at| copies.get(at))
            .map(|copied| (copied.at, copied.src as usize))
            .filter(|&(at, src)| {
                at == range.start
                    && ranges
                        .get(src)
                        .copied()
                        .flatten()
                        .is_some_and(|source| source.start < at && source.end == at)
                    && passed.get(src) == Some(&false)
            });
        let slot = match inherited {
            Some((_, src)) => {
                if let Some(flag) = passed.get_mut(src) {
                    *flag = true;
                }
                handed += 1;
                slots.get(src).copied().ok_or(Fault::Malformed)?
            }
            None => pools[usize::from(uniform)].take(words)?,
        };
        if let Some(s) = slots.get_mut(index) {
            *s = slot;
        }
        live.try_reserve(1).map_err(|_| Fault::OutOfMemory)?;
        live.push(Reverse((range.end, index)));
    }
    let [varying, uniform] = pools;
    Ok(Assigned {
        slots,
        varying_top: varying.top,
        uniform_top: uniform.top - constants,
        handed,
    })
}

impl Lowering<'_> {
    /// Whether `a` and `b` lie in one register: one virtual register, or the
    /// constants, whose words never move apart.
    pub(super) fn one_register(&self, a: Reg, b: Reg) -> bool {
        match (
            virtual_index(&self.virtuals, a),
            virtual_index(&self.virtuals, b),
        ) {
            (Some(x), Some(y)) => x == y,
            (None, None) => super::is_constant(a) && super::is_constant(b),
            _ => false,
        }
    }
}

/// Every virtual register's slot, the slots each class takes, and how many
/// copies handed their source's slot on, which the program then drops.
struct Assigned {
    slots: Vec<u32>,
    varying_top: u32,
    uniform_top: u32,
    handed: usize,
}

/// The register `reg` is given: a virtual register's slot, the scratch
/// planes past every varying one, and a constant's as it is.
fn physical(virtuals: &[(u32, u32, bool)], assigned: &Assigned, reg: Reg) -> Reg {
    let slot = reg.slot();
    if slot >= SCRATCH {
        return Reg::varying(assigned.varying_top + (slot - SCRATCH));
    }
    let Some(index) = virtual_index(virtuals, reg) else {
        return reg;
    };
    let (first, _, _) = virtuals.get(index).copied().unwrap_or((0, 0, false));
    let base = assigned.slots.get(index).copied().unwrap_or(0);
    let at = base + (slot - VIRTUAL - first);
    if reg.is_uniform() {
        Reg::uniform(at)
    } else {
        Reg::varying(at)
    }
}

/// The program's instructions, blocks, edges, moves and switch cases.
#[derive(Default)]
struct Rebuilt {
    insts: Vec<Inst>,
    blocks: Vec<Block>,
    edges: Vec<ir::Edge>,
    moves: Vec<Move>,
    cases: Vec<(u64, u32)>,
}

/// The program built from the laid-out blocks, every register mapped and the
/// `handed` copies that pass a slot on dropped.
fn rebuild(
    mut laid: Laid,
    (steps, images): (&mut [Step], &mut [Image]),
    map: &dyn Fn(Reg) -> Reg,
    handed: usize,
) -> Result<Rebuilt, Fault> {
    let mut rebuilt = Rebuilt::default();
    let mut place_of = Vec::new();
    place_of
        .try_reserve_exact(laid.places.len())
        .map_err(|_| Fault::OutOfMemory)?;
    place_of.extend_from_slice(&laid.places);
    place_of.sort_unstable_by_key(|&(segment, _)| segment);
    let resolve = |segment: Segment| -> Result<u32, Fault> {
        place_of
            .binary_search_by_key(&segment, |&(placed, _)| placed)
            .ok()
            .and_then(|at| place_of.get(at))
            .map(|&(_, place)| place)
            .ok_or(Fault::Malformed)
    };
    reserve_exactly(&mut rebuilt, &laid, handed)?;
    for block in laid.blocks.drain(..) {
        let first = next_index(&rebuilt.insts)?;
        for mut inst in block.insts {
            visit(&mut inst, (steps, images), &mut |reg, _| *reg = map(*reg));
            // A copy whose source handed it its slot moves nothing.
            if matches!(inst, Inst::Copy { dst, src, .. } if dst == src) {
                continue;
            }
            push(&mut rebuilt.insts, inst, MAX_PROGRAM_INSTRUCTIONS)?;
        }
        let last = next_index(&rebuilt.insts)?;
        let term = terminator(block.term, &mut rebuilt, map, &resolve)?;
        push(
            &mut rebuilt.blocks,
            Block {
                insts: (first, last),
                term,
            },
            MAX_PROGRAM_INSTRUCTIONS,
        )?;
    }
    Ok(rebuilt)
}

/// Room in `rebuilt` for exactly what `laid` holds less the `dropped` copies,
/// so the program keeps no slack and its tables are not grown by copying.
fn reserve_exactly(rebuilt: &mut Rebuilt, laid: &Laid, dropped: usize) -> Result<(), Fault> {
    let (mut insts, mut edges, mut moves, mut cases) = (0usize, 0usize, 0usize, 0usize);
    for block in &laid.blocks {
        insts = insts.saturating_add(block.insts.len());
        let mut taken = |edge: &Edge| {
            edges = edges.saturating_add(1);
            moves = moves.saturating_add(edge.moves.len());
        };
        match &block.term {
            Term::Branch(edge) | Term::Barrier(edge) => taken(edge),
            Term::Conditional { then, other, .. } => {
                taken(then);
                taken(other);
            }
            Term::Switch {
                edges: each,
                cases: list,
                ..
            } => {
                each.iter().for_each(&mut taken);
                cases = cases.saturating_add(list.len());
            }
            Term::Exit | Term::Kill => {}
        }
    }
    let exhausted = |_| Fault::OutOfMemory;
    rebuilt
        .insts
        .try_reserve_exact(insts.saturating_sub(dropped))
        .map_err(exhausted)?;
    rebuilt
        .blocks
        .try_reserve_exact(laid.blocks.len())
        .map_err(exhausted)?;
    rebuilt.edges.try_reserve_exact(edges).map_err(exhausted)?;
    rebuilt.moves.try_reserve_exact(moves).map_err(exhausted)?;
    rebuilt.cases.try_reserve_exact(cases).map_err(exhausted)
}

/// A laid-out terminator as the program holds it, its edges and their
/// copies filed and its cases sorted.
fn terminator(
    term: Term,
    rebuilt: &mut Rebuilt,
    map: &dyn Fn(Reg) -> Reg,
    resolve: &dyn Fn(Segment) -> Result<u32, Fault>,
) -> Result<ir::Term, Fault> {
    let edge = |taken: Edge, rebuilt: &mut Rebuilt| -> Result<u32, Fault> {
        let from = next_index(&rebuilt.moves)?;
        for copy in taken.moves {
            let mapped = Move {
                dst: map(copy.dst),
                src: map(copy.src),
                words: copy.words,
            };
            push(&mut rebuilt.moves, mapped, MAX_PROGRAM_INSTRUCTIONS)?;
        }
        let to = next_index(&rebuilt.moves)?;
        let index = next_index(&rebuilt.edges)?;
        let filed = ir::Edge {
            target: resolve(taken.target)?,
            copies: (from, to),
        };
        push(&mut rebuilt.edges, filed, MAX_PROGRAM_INSTRUCTIONS)?;
        Ok(index)
    };
    Ok(match term {
        Term::Branch(taken) => ir::Term::Branch(edge(taken, rebuilt)?),
        Term::Barrier(taken) => ir::Term::Barrier(edge(taken, rebuilt)?),
        Term::Conditional { cond, then, other } => ir::Term::Conditional {
            cond: map(cond),
            then: edge(then, rebuilt)?,
            other: edge(other, rebuilt)?,
        },
        Term::Switch {
            selector,
            wide,
            edges,
            cases: list,
            default,
        } => {
            let mut filed = Vec::new();
            for taken in edges {
                let index = edge(taken, rebuilt)?;
                push(&mut filed, index, MAX_PROGRAM_INSTRUCTIONS)?;
            }
            let resolved = |edge: u32| filed.get(edge as usize).copied().ok_or(Fault::Malformed);
            let default = resolved(default)?;
            let mut sorted = Vec::new();
            for (literal, target) in list {
                push(
                    &mut sorted,
                    (literal, resolved(target)?),
                    MAX_PROGRAM_INSTRUCTIONS,
                )?;
            }
            sorted.sort_unstable_by_key(|&(literal, _)| literal);
            sorted.dedup_by_key(|&mut (literal, _)| literal);
            let from = next_index(&rebuilt.cases)?;
            for case in sorted {
                push(&mut rebuilt.cases, case, MAX_PROGRAM_INSTRUCTIONS)?;
            }
            let to = next_index(&rebuilt.cases)?;
            ir::Term::Switch {
                selector: map(selector),
                wide,
                cases: (from, to),
                default,
            }
        }
        Term::Exit => ir::Term::Exit,
        Term::Kill => ir::Term::Kill,
    })
}

/// One class's slots: those freed, a list for each size, and the next
/// never used. The common sizes index an array; the rest a tree, so a
/// module of many sizes costs no scan.
struct Pool {
    small: [Vec<u32>; SMALL],
    large: RadixTree<Vec<u32>>,
    top: u32,
}

/// The sizes, in words, freed slots are kept by size for.
const SMALL: usize = 32;

impl Pool {
    fn new(top: u32) -> Self {
        Self {
            small: core::array::from_fn(|_| Vec::new()),
            large: RadixTree::new(),
            top,
        }
    }

    fn take(&mut self, words: u32) -> Result<u32, Fault> {
        let reused = match self.small.get_mut(words as usize) {
            Some(list) => list.pop(),
            None => self.large.get_mut(u64::from(words)).and_then(Vec::pop),
        };
        if let Some(slot) = reused {
            return Ok(slot);
        }
        let slot = self.top;
        self.top = slot.checked_add(words).ok_or(Fault::Budget)?;
        if self.top >= SCRATCH - VIRTUAL {
            return Err(Fault::Budget);
        }
        Ok(slot)
    }

    fn free(&mut self, slot: u32, words: u32) -> Result<(), Fault> {
        if let Some(list) = self.small.get_mut(words as usize) {
            return push(list, slot, MAX_PROGRAM_INSTRUCTIONS * 4);
        }
        let size = u64::from(words);
        if let Some(list) = self.large.get_mut(size) {
            return push(list, slot, MAX_PROGRAM_INSTRUCTIONS * 4);
        }
        let mut list = Vec::new();
        push(&mut list, slot, MAX_PROGRAM_INSTRUCTIONS * 4)?;
        self.large
            .try_insert(size, list)
            .map_err(|_| Fault::OutOfMemory)?;
        Ok(())
    }
}

/// Each laid-out block's first position: its instructions in turn, then its
/// terminator.
fn positions(laid: &Laid) -> Result<Vec<u32>, Fault> {
    let mut starts = Vec::new();
    starts
        .try_reserve_exact(laid.blocks.len())
        .map_err(|_| Fault::OutOfMemory)?;
    let mut at = 0u32;
    for block in &laid.blocks {
        starts.push(at);
        let length = next_index(&block.insts)?;
        at = at
            .checked_add(length)
            .and_then(|a| a.checked_add(1))
            .ok_or(Fault::Budget)?;
    }
    Ok(starts)
}

/// Each register a terminator names.
fn visit_term(term: &mut Term, f: &mut dyn FnMut(&mut Reg, bool)) {
    let edge = |e: &mut Edge, f: &mut dyn FnMut(&mut Reg, bool)| {
        for m in &mut e.moves {
            f(&mut m.src, false);
            f(&mut m.dst, true);
        }
    };
    match term {
        Term::Branch(e) | Term::Barrier(e) => edge(e, f),
        Term::Conditional { cond, then, other } => {
            f(cond, false);
            edge(then, f);
            edge(other, f);
        }
        Term::Switch {
            selector, edges, ..
        } => {
            f(selector, false);
            for e in edges {
                edge(e, f);
            }
        }
        Term::Exit | Term::Kill => {}
    }
}

/// The virtual register `reg` is a word of, by its place among them.
fn virtual_index(virtuals: &[(u32, u32, bool)], reg: Reg) -> Option<usize> {
    let slot = reg.slot();
    if !(VIRTUAL..SCRATCH).contains(&slot) {
        return None;
    }
    let word = slot - VIRTUAL;
    let after = virtuals.partition_point(|&(first, _, _)| first <= word);
    let index = after.checked_sub(1)?;
    let (first, words, _) = *virtuals.get(index)?;
    (word < first + words).then_some(index)
}

/// The virtual registers of `inst` where it copies the whole of one into
/// another of the same size and class: the destination, then the source.
fn whole_copy(virtuals: &[(u32, u32, bool)], inst: &Inst) -> Option<(usize, usize)> {
    let Inst::Copy { dst, src, words } = *inst else {
        return None;
    };
    let whole = |reg: Reg| {
        let index = virtual_index(virtuals, reg)?;
        let (first, size, uniform) = *virtuals.get(index)?;
        (reg.slot() == VIRTUAL + first && size == words && reg.is_uniform() == uniform)
            .then_some((index, uniform))
    };
    let ((to, to_class), (from, from_class)) = (whole(dst)?, whole(src)?);
    (to != from && to_class == from_class).then_some((to, from))
}

/// Widen each range over the loops other lanes may run again while a lane
/// still holds its value: one defined before a loop and used within it to
/// the loop's end, and, since a tile's shared registers are written
/// whichever lanes run, a shared one defined within a loop and used past it
/// to the loop's start.
fn widen(
    ranges: &mut [Option<Range>],
    virtuals: &[(u32, u32, bool)],
    laid: &Laid,
    starts: &[u32],
) -> Result<(), Fault> {
    if laid.loops.is_empty() {
        return Ok(());
    }
    // Each loop's span of blocks and of positions.
    let mut loops: Vec<(u32, u32, u32, u32)> = Vec::new();
    for &(header, end) in &laid.loops {
        let first = starts
            .get(header as usize)
            .copied()
            .ok_or(Fault::Malformed)?;
        let block = laid.blocks.get(end as usize).ok_or(Fault::Malformed)?;
        let last =
            starts.get(end as usize).copied().ok_or(Fault::Malformed)? + next_index(&block.insts)?;
        push(
            &mut loops,
            (first, last, header, end),
            MAX_PROGRAM_INSTRUCTIONS,
        )?;
    }
    loops.sort_unstable_by_key(|&(first, last, _, _)| (first, core::cmp::Reverse(last)));
    // Each loop's enclosing loop, and each block's innermost, in one sweep.
    let mut parent = filled(loops.len(), NO_LOOP)?;
    let mut innermost = filled(laid.blocks.len(), NO_LOOP)?;
    let mut open: Vec<u32> = Vec::new();
    let mut next = 0usize;
    for (block, held) in (0u32..).zip(innermost.iter_mut()) {
        while let Some(&top) = open.last() {
            if loops
                .get(top as usize)
                .is_some_and(|&(_, _, _, end)| end >= block)
            {
                break;
            }
            open.pop();
        }
        while let Some(&(_, _, header, _)) = loops.get(next) {
            if header != block {
                break;
            }
            if let Some(up) = parent.get_mut(next) {
                *up = open.last().copied().unwrap_or(NO_LOOP);
            }
            push(
                &mut open,
                u32::try_from(next).map_err(|_| Fault::Budget)?,
                MAX_PROGRAM_INSTRUCTIONS,
            )?;
            next += 1;
        }
        *held = open.last().copied().unwrap_or(NO_LOOP);
    }
    let innermost_at = |at: u32| -> u32 {
        starts
            .partition_point(|&start| start <= at)
            .checked_sub(1)
            .and_then(|block| innermost.get(block))
            .copied()
            .unwrap_or(NO_LOOP)
    };
    let span = |l: u32| {
        loops
            .get(l as usize)
            .map(|&(first, last, _, _)| (first, last))
    };
    let up = |l: u32| parent.get(l as usize).copied().unwrap_or(NO_LOOP);
    let mut steps = 0usize;
    let mut step = || {
        steps += 1;
        if steps > MAX_WIDENING_STEPS {
            Err(Fault::Budget)
        } else {
            Ok(())
        }
    };
    for (range, &(_, _, shared)) in ranges.iter_mut().zip(virtuals) {
        let Some(range) = range else {
            continue;
        };
        let mut l = innermost_at(range.end);
        while let Some((first, last)) = span(l) {
            step()?;
            if first <= range.start {
                break;
            }
            range.end = range.end.max(last);
            l = up(l);
        }
        if shared {
            let mut l = innermost_at(range.start);
            while let Some((first, last)) = span(l) {
                step()?;
                if last >= range.end {
                    break;
                }
                range.start = range.start.min(first);
                l = up(l);
            }
        }
    }
    Ok(())
}

/// No loop: past every loop's index.
const NO_LOOP: u32 = u32::MAX;

#[cfg(test)]
#[path = "regs_tests.rs"]
mod tests;

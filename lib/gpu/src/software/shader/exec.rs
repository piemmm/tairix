//! A tile of up to [`LANES`] lanes running a program: each instruction over
//! the lanes of the block it is in, every value a plane of words per lane or
//! one the tile shares, and the lanes' blocks kept in structured order so
//! they reconverge at every merge.

mod group;
mod image;
mod memory;
mod vector;

use core::ops::Range;

use super::ir::{Block, Builtin, Edge, Inst, Num, Op1, Program, Reg, Term, LANES};
use super::ops::{self, high32, low32};
use crate::software::texel::Srgb;
use crate::software::Meter;

pub(crate) use memory::{ImageAt, Resolved};

/// How a tile's run ended.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Ran {
    /// Every lane ended.
    Finished,
    /// The tile's lanes reached a barrier its workgroup's other tiles must
    /// reach too.
    Barrier,
    /// The dispatch was stopped.
    Stopped,
    /// The program proved broken: what lowering admitted, the machine could
    /// not run.
    Fault,
}

/// What every tile of a dispatch reads and never writes: the program, its
/// resources and the device's tables.
pub(crate) struct Env<'d> {
    pub(crate) program: &'d Program,
    /// Each of the program's bindings, element by element.
    pub(crate) bindings: &'d [&'d [Resolved<'d>]],
    pub(crate) push: &'d [u32],
    pub(crate) srgb: &'d Srgb,
    pub(crate) subgroup_size: u32,
    pub(crate) groups: [u32; 3],
}

/// Where each lane of a tile is: its invocation within its workgroup, its
/// workgroup, and the lanes of its subgroup.
#[derive(Clone, Debug)]
pub(crate) struct Placement {
    pub(crate) live: u64,
    pub(crate) local: [u32; LANES],
    /// The ids of the tile's workgroups, by their place among them.
    pub(crate) group_id: [[u32; 3]; LANES],
    /// Each lane's workgroup, by its place among the tile's.
    pub(crate) group_slot: [u32; LANES],
    /// The first lane of each lane's subgroup, and how many it holds.
    pub(crate) subgroup: [(u8, u8); LANES],
}

impl Placement {
    /// A placement of no lanes.
    pub(crate) const EMPTY: Self = Self {
        live: 0,
        local: [0; LANES],
        group_id: [[0; 3]; LANES],
        group_slot: [0; LANES],
        subgroup: [(0, 0); LANES],
    };
}

/// A tile's own memory, which holds it between runs: its register planes
/// and computed shared words, its lanes' private words, the words its lanes
/// share, its blocks' lanes, its lanes still running, and its workgroups'
/// memory.
pub(crate) struct Space<'w> {
    pub(crate) planes: &'w mut [u32],
    pub(crate) uniforms: &'w mut [u32],
    pub(crate) private: &'w mut [u32],
    pub(crate) shared: &'w mut [u32],
    pub(crate) masks: &'w mut [u64],
    pub(crate) pending: &'w mut [u64],
    pub(crate) live: &'w mut u64,
    pub(crate) workgroup: &'w mut [u32],
}

/// A tile running.
pub(crate) struct Machine<'r, 'd, 'w> {
    pub(crate) env: &'r Env<'d>,
    pub(crate) place: &'r Placement,
    pub(crate) space: Space<'w>,
    pub(crate) meter: &'r mut Meter<'d>,
    /// The lanes of the block running, and the span from its first to its
    /// last; one lane wide for an instruction of the tile's.
    mask: u64,
    span: Range<usize>,
    /// No block before this one has lanes waiting.
    earliest: usize,
}

/// One value's lanes, widened to a 64-bit word apiece.
pub(crate) type Lanes = [u64; LANES];

/// The lanes of `mask`, lowest first.
pub(crate) fn each_lane(mask: u64) -> impl Iterator<Item = usize> {
    let mut left = mask;
    core::iter::from_fn(move || {
        if left == 0 {
            return None;
        }
        let lane = left.trailing_zeros() as usize;
        left &= left - 1;
        Some(lane)
    })
}

/// The span from the first to the last lane of `mask`.
fn span_of(mask: u64) -> Range<usize> {
    if mask == 0 {
        return 0..0;
    }
    let first = mask.trailing_zeros() as usize;
    let last = 63 - mask.leading_zeros() as usize;
    first..last + 1
}

impl<'r, 'd, 'w> Machine<'r, 'd, 'w> {
    /// A tile over `space`, as it was left.
    pub(crate) fn new(
        env: &'r Env<'d>,
        place: &'r Placement,
        space: Space<'w>,
        meter: &'r mut Meter<'d>,
    ) -> Self {
        Self {
            env,
            place,
            space,
            meter,
            mask: place.live,
            span: span_of(place.live),
            earliest: 0,
        }
    }

    /// Start the tile: its memory zeroed, its built-ins given, and every lane
    /// at the entry block, once the clock allows: `false` where the dispatch
    /// is stopped instead. Its blocks have no lanes waiting already: a piece
    /// clears them before its first tile, and a tile ends only once none has.
    pub(crate) fn begin(&mut self) -> bool {
        if !self.meter.proceed(0) {
            return false;
        }
        self.space.private.fill(0);
        self.space.shared.fill(0);
        let cleared = self.space.private.len() + self.space.shared.len();
        let given = self.env.program.given.len() * 4 * LANES;
        self.meter.charge((cleared + given) as u64);
        *self.space.live = self.place.live;
        self.give();
        self.arrive(0, self.place.live);
        true
    }

    /// Run until every lane ends, the lanes reach a barrier, the dispatch is
    /// stopped, or the program proves broken. The clock is consulted only
    /// before work, so a tile whose last work is done is never stopped.
    pub(crate) fn run(&mut self) -> Ran {
        let program = self.env.program;
        loop {
            let Some(at) = self.next_block() else {
                return Ran::Finished;
            };
            let Some(block) = program.blocks.get(at) else {
                return Ran::Fault;
            };
            let lanes = self.space.masks.get(at).copied().unwrap_or(0) & *self.space.live;
            // An empty block that ends its lanes leaves no work to stop
            // before, and is in no loop, so one a lane at most runs between
            // two looks at the clock; a loop of empty blocks still reaches it.
            let ends = matches!(block.term, Term::Exit | Term::Kill)
                && program.inst_range(block.insts).is_empty();
            if !ends && !self.meter.proceed(0) {
                return Ran::Stopped;
            }
            self.leave(at);
            if lanes == 0 {
                continue;
            }
            match self.block(block, lanes) {
                Some(Ran::Finished) => {}
                Some(ran) => return ran,
                None => return Ran::Fault,
            }
        }
    }

    /// Run one block's instructions and its terminator over `lanes`;
    /// `None` where the program proved broken. Work is counted over what
    /// each instruction computes: the span the lanes cover, or one lane for a
    /// value the tile shares.
    fn block(&mut self, block: &Block, lanes: u64) -> Option<Ran> {
        let program = self.env.program;
        let width = span_of(lanes).len() as u64;
        for (at, inst) in program.inst_range(block.insts).iter().enumerate() {
            // The block's first instruction follows the check made as it began.
            if at > 0 && !self.meter.proceed(0) {
                return Some(Ran::Stopped);
            }
            self.focus(lanes);
            let over = if is_uniform(inst) {
                self.mask = 1;
                self.span = 0..1;
                1
            } else {
                width
            };
            self.execute(inst)?;
            self.meter.charge(inst.work(program).saturating_mul(over));
        }
        self.focus(lanes);
        match block.term {
            Term::Branch(edge) => self.take(edge, lanes)?,
            Term::Conditional { cond, then, other } => {
                let taken = self.truth(cond, lanes);
                self.take(then, taken)?;
                self.take(other, lanes & !taken)?;
            }
            Term::Switch {
                selector,
                wide,
                cases,
                default,
            } => self.switch(selector, wide, cases, default, lanes)?,
            Term::Exit | Term::Kill => *self.space.live &= !lanes,
            Term::Barrier(edge) => {
                self.take(edge, lanes)?;
                self.meter.charge(width);
                return Some(Ran::Barrier);
            }
        }
        self.meter.charge(width);
        Some(Ran::Finished)
    }

    fn focus(&mut self, lanes: u64) {
        self.mask = lanes;
        self.span = span_of(lanes);
    }

    /// The earliest block with lanes waiting, found from the last one so a
    /// program of many blocks is not scanned from its start at every step;
    /// the words looked at are work like any other.
    fn next_block(&mut self) -> Option<usize> {
        let from = self.earliest / 64;
        let (word, bits) = self
            .space
            .pending
            .iter()
            .enumerate()
            .skip(from)
            .find(|(_, bits)| **bits != 0)?;
        self.meter.charge((word + 1 - from) as u64);
        self.earliest = word * 64 + bits.trailing_zeros() as usize;
        Some(self.earliest)
    }

    fn leave(&mut self, at: usize) {
        if let Some(mask) = self.space.masks.get_mut(at) {
            *mask = 0;
        }
        if let Some(bits) = self.space.pending.get_mut(at / 64) {
            *bits &= !(1 << (at % 64));
        }
    }

    fn arrive(&mut self, at: usize, lanes: u64) {
        if lanes == 0 {
            return;
        }
        self.earliest = self.earliest.min(at);
        if let Some(mask) = self.space.masks.get_mut(at) {
            *mask |= lanes;
        }
        if let Some(bits) = self.space.pending.get_mut(at / 64) {
            *bits |= 1 << (at % 64);
        }
    }

    /// Send `lanes` along `edge`, making its copies for them.
    fn take(&mut self, edge: u32, lanes: u64) -> Option<()> {
        if lanes == 0 {
            return Some(());
        }
        let program = self.env.program;
        let Edge { target, copies } = *program.edges.get(edge as usize)?;
        self.focus(lanes);
        let moves = program.moves.get(copies.0 as usize..copies.1 as usize)?;
        let width = self.span.len() as u64;
        for step in moves {
            self.copy(step.dst, step.src, step.words);
            self.meter.charge(u64::from(step.words) * width);
        }
        self.arrive(target as usize, lanes);
        Some(())
    }

    /// The lanes of `lanes` whose Boolean `cond` holds.
    fn truth(&self, cond: Reg, lanes: u64) -> u64 {
        let mut values = [0; LANES];
        self.fetch(cond, 0, false, &mut values);
        let mut taken = 0;
        for lane in self.span.clone() {
            if values.get(lane).copied().unwrap_or(0) & 1 != 0 {
                taken |= 1 << lane;
            }
        }
        taken & lanes
    }

    fn switch(
        &mut self,
        selector: Reg,
        wide: bool,
        cases: (u32, u32),
        default: u32,
        lanes: u64,
    ) -> Option<()> {
        let program = self.env.program;
        let table = program.cases.get(cases.0 as usize..cases.1 as usize)?;
        let mut values = [0; LANES];
        self.fetch(selector, 0, wide, &mut values);
        let narrow = if wide { u64::MAX } else { u64::from(u32::MAX) };
        let mut routes = [(default, 0usize); LANES];
        let mut count = 0;
        for lane in each_lane(lanes) {
            let value = values.get(lane).copied().unwrap_or(0) & narrow;
            let edge = table
                .binary_search_by_key(&value, |&(literal, _)| literal)
                .ok()
                .and_then(|at| table.get(at))
                .map_or(default, |&(_, edge)| edge);
            if let Some(route) = routes.get_mut(count) {
                *route = (edge, lane);
                count += 1;
            }
        }
        let routes = routes.get_mut(..count)?;
        // Each edge's lanes are sent at once, so its copies are made once.
        routes.sort_unstable();
        let searched = (table.len() + 1).ilog2() + 1;
        let sorted = (count + 1).ilog2() + 1;
        self.meter
            .charge(count as u64 * u64::from(searched.saturating_add(sorted)));
        for run in routes.chunk_by(|a, b| a.0 == b.0) {
            let same = run.iter().fold(0u64, |same, &(_, lane)| same | 1 << lane);
            if let Some(&(edge, _)) = run.first() {
                self.take(edge, same)?;
            }
        }
        Some(())
    }

    /// Word `word` (and the next, where `wide`) of `reg` for each lane of
    /// the span: a uniform register's one value in every lane.
    pub(crate) fn fetch(&self, reg: Reg, word: u32, wide: bool, out: &mut Lanes) {
        let slot = (reg.slot() + word) as usize;
        let span = self.span.clone();
        let Some(out) = out.get_mut(span.clone()) else {
            return;
        };
        if reg.is_uniform() {
            let low = self.shared_word(slot);
            let high = if wide { self.shared_word(slot + 1) } else { 0 };
            out.fill(u64::from(low) | u64::from(high) << 32);
            return;
        }
        let plane = |at: usize| {
            let base = at * LANES;
            self.space.planes.get(base + span.start..base + span.end)
        };
        match (plane(slot), if wide { plane(slot + 1) } else { None }) {
            (Some(low), Some(high)) => {
                for ((value, &low), &high) in out.iter_mut().zip(low).zip(high) {
                    *value = u64::from(low) | u64::from(high) << 32;
                }
            }
            (Some(low), None) => {
                for (value, &low) in out.iter_mut().zip(low) {
                    *value = u64::from(low);
                }
            }
            (None, _) => out.fill(0),
        }
    }

    /// Word `word` of `reg` in lane `lane`: a uniform register's one value
    /// in every lane.
    pub(crate) fn word(&self, reg: Reg, word: u32, lane: usize) -> u64 {
        let slot = (reg.slot() + word) as usize;
        if reg.is_uniform() {
            return u64::from(self.shared_word(slot));
        }
        self.space
            .planes
            .get(slot * LANES + lane)
            .copied()
            .map_or(0, u64::from)
    }

    /// Write word `word` (and the next, where `wide`) of `reg` from each
    /// lane of the span the mask holds; a uniform register takes the span's
    /// first lane's value.
    pub(crate) fn store(&mut self, reg: Reg, word: u32, wide: bool, values: &Lanes) {
        let slot = (reg.slot() + word) as usize;
        if reg.is_uniform() {
            let value = values.get(self.span.start).copied().unwrap_or(0);
            let computed = slot.wrapping_sub(self.env.program.constants.len());
            if let Some(low) = self.space.uniforms.get_mut(computed) {
                *low = low32(value);
            }
            if wide {
                if let Some(high) = self.space.uniforms.get_mut(computed.wrapping_add(1)) {
                    *high = high32(value);
                }
            }
            return;
        }
        let span = self.span.clone();
        let Some(values) = values.get(span.clone()) else {
            return;
        };
        let held = self.mask >> span.start;
        let full = self.mask == span_mask(&span);
        for half in 0..=usize::from(wide) {
            let base = (slot + half) * LANES;
            let Some(plane) = self
                .space
                .planes
                .get_mut(base + span.start..base + span.end)
            else {
                return;
            };
            let shift = 32 * half;
            if full {
                for (into, &value) in plane.iter_mut().zip(values) {
                    *into = low32(value >> shift);
                }
            } else {
                for (at, (into, &value)) in plane.iter_mut().zip(values).enumerate() {
                    if held >> at & 1 != 0 {
                        *into = low32(value >> shift);
                    }
                }
            }
        }
    }

    /// Word `slot` of the tile's shared registers: a constant's, held by the
    /// program, or one the tile computed.
    fn shared_word(&self, slot: usize) -> u32 {
        let constants = &self.env.program.constants;
        match slot.checked_sub(constants.len()) {
            None => constants.get(slot).copied().unwrap_or(0),
            Some(computed) => self.space.uniforms.get(computed).copied().unwrap_or(0),
        }
    }

    /// Copy `words` words of `src` into `dst` for the lanes in focus.
    pub(crate) fn copy(&mut self, dst: Reg, src: Reg, words: u32) {
        let span = self.span.clone();
        let full = self.mask == span_mask(&span);
        let mut values = [0; LANES];
        for word in 0..words {
            let (from, to) = ((src.slot() + word) as usize, (dst.slot() + word) as usize);
            let planes = &mut self.space.planes;
            let held = |slot: usize| slot.saturating_add(1).saturating_mul(LANES) <= planes.len();
            // Plane to plane, as both are held; a uniform or missing one goes
            // the long way, widened and narrowed.
            if !src.is_uniform() && !dst.is_uniform() && held(from) && held(to) {
                let (from, to) = (from * LANES, to * LANES);
                if full {
                    planes.copy_within(from + span.start..from + span.end, to + span.start);
                } else {
                    for lane in each_lane(self.mask) {
                        let value = planes.get(from + lane).copied();
                        if let (Some(value), Some(into)) = (value, planes.get_mut(to + lane)) {
                            *into = value;
                        }
                    }
                }
                continue;
            }
            self.fetch(src, word, false, &mut values);
            self.store(dst, word, false, &values);
        }
    }

    pub(crate) fn zero(&mut self, dst: Reg, words: u32) {
        let values = [0; LANES];
        for word in 0..words {
            self.store(dst, word, false, &values);
        }
    }

    /// `kernel` over each component of `a`, of `from`, into components of
    /// `to`.
    fn map1(
        &mut self,
        (from, to): (Num, Num),
        count: u32,
        dst: Reg,
        a: Reg,
        kernel: impl Fn(&mut [u64]),
    ) {
        let (in_words, out_words) = (from.words(), to.words());
        let span = self.span.clone();
        let mut values = [0; LANES];
        for component in 0..count {
            self.fetch(a, component * in_words, in_words == 2, &mut values);
            if let Some(lanes) = values.get_mut(span.clone()) {
                kernel(lanes);
            }
            self.store(dst, component * out_words, out_words == 2, &values);
        }
    }

    /// `kernel` over each component of `a` and `b`, of the operands' types,
    /// into components of `to`.
    fn map2(
        &mut self,
        (of_a, of_b): (Num, Num),
        to: Num,
        count: u32,
        dst: Reg,
        (a, b): (Reg, Reg),
        kernel: impl Fn(&mut [u64], &[u64]),
    ) {
        let (a_words, b_words, out_words) = (of_a.words(), of_b.words(), to.words());
        let span = self.span.clone();
        let (mut first, mut second) = ([0; LANES], [0; LANES]);
        for component in 0..count {
            self.fetch(a, component * a_words, a_words == 2, &mut first);
            self.fetch(b, component * b_words, b_words == 2, &mut second);
            if let (Some(xs), Some(ys)) = (first.get_mut(span.clone()), second.get(span.clone())) {
                kernel(xs, ys);
            }
            self.store(dst, component * out_words, out_words == 2, &first);
        }
    }

    fn map3(
        &mut self,
        num: Num,
        count: u32,
        dst: Reg,
        (a, b, c): (Reg, Reg, Reg),
        kernel: impl Fn(&mut [u64], &[u64], &[u64]),
    ) {
        let words = num.words();
        let wide = words == 2;
        let span = self.span.clone();
        let (mut first, mut second, mut third) = ([0; LANES], [0; LANES], [0; LANES]);
        for component in 0..count {
            let at = component * words;
            self.fetch(a, at, wide, &mut first);
            self.fetch(b, at, wide, &mut second);
            self.fetch(c, at, wide, &mut third);
            if let (Some(xs), Some(ys), Some(zs)) = (
                first.get_mut(span.clone()),
                second.get(span.clone()),
                third.get(span.clone()),
            ) {
                kernel(xs, ys, zs);
            }
            self.store(dst, at, wide, &first);
        }
    }

    /// Run one instruction over the lanes in focus; `None` where the
    /// program proved broken.
    #[expect(clippy::too_many_lines, reason = "one arm per lowered instruction")]
    fn execute(&mut self, inst: &Inst) -> Option<()> {
        match *inst {
            Inst::Copy { dst, src, words } => self.copy(dst, src, words),
            Inst::Zero { dst, words } => self.zero(dst, words),
            Inst::Map1 {
                op: op @ (Op1::IsNan | Op1::IsInf),
                num,
                count,
                dst,
                a,
            } => {
                let kernel = ops::unary(op);
                self.map1((num, Num::Bool), count, dst, a, |xs| kernel(num, xs));
            }
            Inst::Map1 {
                op,
                num,
                count,
                dst,
                a,
            } => {
                let kernel = ops::unary(op);
                self.map1((num, num), count, dst, a, |xs| kernel(num, xs));
            }
            Inst::Map2 {
                op,
                num,
                count,
                dst,
                a,
                b,
            } => {
                let kernel = ops::binary(op);
                self.map2((num, num), num, count, dst, (a, b), |xs, ys| {
                    kernel(num, xs, ys);
                });
            }
            Inst::Map3 {
                op,
                num,
                count,
                dst,
                a,
                b,
                c,
            } => {
                let kernel = ops::ternary(op);
                self.map3(num, count, dst, (a, b, c), |xs, ys, zs| {
                    kernel(num, xs, ys, zs);
                });
            }
            Inst::Shift {
                op,
                num,
                by,
                count,
                dst,
                a,
                b,
            } => {
                let kernel = ops::shifter(op);
                self.map2((num, by), num, count, dst, (a, b), |xs, ys| {
                    kernel(num, by, xs, ys);
                });
            }
            Inst::Compare {
                op,
                num,
                count,
                dst,
                a,
                b,
            } => {
                let kernel = ops::comparer(op);
                self.map2((num, num), Num::Bool, count, dst, (a, b), |xs, ys| {
                    kernel(num, xs, ys);
                });
            }
            Inst::Convert {
                op,
                from,
                to,
                count,
                dst,
                a,
            } => {
                let kernel = ops::converter(op);
                self.map1((from, to), count, dst, a, |xs| kernel(from, to, xs));
            }
            Inst::Select {
                dst,
                cond,
                a,
                b,
                words,
                per,
            } => self.select(dst, cond, (a, b), words, per),
            Inst::AnyAll { all, count, dst, a } => self.any_all(all, count, dst, a),
            Inst::Linear {
                op,
                num,
                dst,
                a,
                b,
                c,
            } => self.linear(op, num, dst, (a, b, c)),
            Inst::Pack { op, dst, a } => self.pack(op, dst, a),
            Inst::Wide {
                op,
                num,
                count,
                dst,
                second,
                a,
                b,
            } => self.wide(op, num, count, (dst, second), (a, b)),
            Inst::BitField {
                op,
                num,
                by,
                count,
                dst,
                base,
                insert,
                offset,
                bits,
            } => self.bit_field(op, (num, by), count, dst, (base, insert, offset, bits)),
            Inst::Ldexp {
                num,
                by,
                count,
                dst,
                a,
                b,
            } => self.ldexp((num, by), count, dst, (a, b)),
            Inst::Extract {
                dst,
                vector,
                index,
                by,
                count,
                per,
            } => self.extract(dst, vector, (index, by), count, per),
            Inst::Insert {
                dst,
                vector,
                value,
                index,
                by,
                count,
                per,
            } => self.insert(dst, (vector, value), (index, by), count, per),
            Inst::Chain { dst, base, steps } => self.chain(dst, base, steps)?,
            Inst::Load {
                dst,
                root,
                ptr,
                plan,
            } => self.load(dst, root, ptr, plan)?,
            Inst::Store {
                src,
                root,
                ptr,
                plan,
            } => self.store_memory(src, root, ptr, plan)?,
            Inst::ArrayLength {
                dst,
                root,
                ptr,
                offset,
                stride,
            } => self.array_length(dst, root, ptr, (offset, stride)),
            Inst::Atomic {
                op,
                order,
                root,
                ptr,
                dst,
                a,
                b,
            } => self.atomic(op, order, root, ptr, dst, (a, b)),
            Inst::TexelPointer {
                dst,
                image,
                handle,
                coord,
                coords,
                sample,
            } => self.texel_pointer(dst, image, handle, (coord, coords), sample),
            Inst::Image(at) => self.image(self.env.program.images.get(at as usize)?),
            Inst::Group { op, dst, a, b } => self.group(op, dst, (a, b)),
            Inst::Derivative {
                op,
                fine,
                num,
                count,
                dst,
                a,
            } => self.derivative(op, fine, (num, count), dst, a),
            Inst::Fence(order) => memory::fence(order),
        }
        Some(())
    }

    fn select(&mut self, dst: Reg, cond: Reg, (a, b): (Reg, Reg), words: u32, per: u32) {
        let (mut holds, mut chosen, mut otherwise) = ([0; LANES], [0; LANES], [0; LANES]);
        let per = per.max(1);
        for word in 0..words {
            // A condition's component is fetched once for all the words it chooses.
            if word % per == 0 {
                self.fetch(cond, word / per, false, &mut holds);
            }
            self.fetch(a, word, false, &mut chosen);
            self.fetch(b, word, false, &mut otherwise);
            for lane in self.span.clone() {
                if let (Some(value), Some(&other), Some(&held)) =
                    (chosen.get_mut(lane), otherwise.get(lane), holds.get(lane))
                {
                    if held & 1 == 0 {
                        *value = other;
                    }
                }
            }
            self.store(dst, word, false, &chosen);
        }
    }

    fn any_all(&mut self, all: bool, count: u32, dst: Reg, a: Reg) {
        let mut out = [u64::from(all); LANES];
        let mut x = [0; LANES];
        for component in 0..count {
            self.fetch(a, component, false, &mut x);
            for lane in self.span.clone() {
                if let (Some(o), Some(&v)) = (out.get_mut(lane), x.get(lane)) {
                    *o = if all { *o & v & 1 } else { *o | (v & 1) };
                }
            }
        }
        self.store(dst, 0, false, &out);
    }

    /// Write each built-in the program reads from memory into its root, for
    /// every lane of the tile.
    fn give(&mut self) {
        let program = self.env.program;
        self.focus(self.place.live);
        for given in &program.given {
            let words = builtin_words(given.which);
            let mut values = [[0u64; LANES]; 4];
            self.builtin_values(given.which, &mut values);
            for (word, lanes) in values.iter().take(words as usize).enumerate() {
                memory::give(self, given.root, word, lanes);
            }
        }
    }
}

/// How many words a built-in holds.
pub(crate) const fn builtin_words(which: Builtin) -> u32 {
    match which {
        Builtin::LocalInvocationId
        | Builtin::GlobalInvocationId
        | Builtin::WorkgroupId
        | Builtin::NumWorkgroups
        | Builtin::WorkgroupSize => 3,
        Builtin::SubgroupEqMask
        | Builtin::SubgroupGeMask
        | Builtin::SubgroupGtMask
        | Builtin::SubgroupLeMask
        | Builtin::SubgroupLtMask => 4,
        _ => 1,
    }
}

/// Whether `inst` computes a value the tile shares, once for every lane.
fn is_uniform(inst: &Inst) -> bool {
    let (Inst::Copy { dst, .. }
    | Inst::Zero { dst, .. }
    | Inst::Map1 { dst, .. }
    | Inst::Map2 { dst, .. }
    | Inst::Map3 { dst, .. }
    | Inst::Shift { dst, .. }
    | Inst::Compare { dst, .. }
    | Inst::Convert { dst, .. }
    | Inst::Select { dst, .. }
    | Inst::AnyAll { dst, .. }
    | Inst::Linear { dst, .. }
    | Inst::Pack { dst, .. }
    | Inst::Wide { dst, .. }
    | Inst::BitField { dst, .. }
    | Inst::Ldexp { dst, .. }
    | Inst::Extract { dst, .. }
    | Inst::Insert { dst, .. }
    | Inst::Chain { dst, .. }
    | Inst::Load { dst, .. }) = *inst
    else {
        return false;
    };
    dst.is_uniform()
}

fn span_mask(span: &Range<usize>) -> u64 {
    let len = span.end.saturating_sub(span.start);
    if len >= 64 {
        u64::MAX
    } else {
        ((1u64 << len) - 1) << span.start
    }
}

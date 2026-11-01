//! The module's functions indexed, its globals declared, and the entry
//! point's blocks laid out in structured order with every call inlined:
//! each construct's blocks before its merge, each loop's construct before
//! its continue construct before its merge, and a call's blocks between the
//! code before it and the code after. Each block is lowered as it is
//! placed, so every value is defined before its first use; a phi's value is
//! made on each edge into its block.

use alloc::vec::Vec;

use tairix_spirv::enumerant::{Scope, StorageClass};
use tairix_spirv::{Id, Instruction, Op, Scalar, Type, Value};

use super::{
    filled, memory, next_index, push, Count, Fault, Lowering, Val, MAX_PROGRAM_INSTRUCTIONS,
};
use crate::software::shader::ir::{Inst, Move, Reg};
use crate::software::shader::ops::mask;

/// The most blocks a lowered program holds.
const MAX_BLOCKS: usize = MAX_PROGRAM_INSTRUCTIONS / 2;

/// The most function instances inlining makes.
const MAX_INSTANCES: usize = MAX_PROGRAM_INSTRUCTIONS / 4;

/// A block being built: its instructions, then where its lanes go.
pub(crate) struct Built {
    pub(crate) insts: Vec<Inst>,
    pub(crate) term: Term,
}

/// A terminator whose edges name the runs they reach before those have
/// places.
pub(crate) enum Term {
    Branch(Edge),
    Conditional {
        cond: Reg,
        then: Edge,
        other: Edge,
    },
    /// A switch's edges, one for each target, and its cases and default
    /// by their edge.
    Switch {
        selector: Reg,
        wide: bool,
        edges: Vec<Edge>,
        cases: Vec<(u64, u32)>,
        default: u32,
    },
    Exit,
    Kill,
    Barrier(Edge),
}

/// An edge: the run it reaches, and the copies it makes.
pub(crate) struct Edge {
    pub(crate) target: Segment,
    pub(crate) moves: Vec<Move>,
}

/// One run of a block's instructions between calls and barriers: the
/// instance, the block within its function, and which run.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct Segment {
    pub(crate) instance: u32,
    pub(crate) block: u32,
    pub(crate) run: u32,
}

/// The blocks laid out, each run's place, and the loops among them: each
/// header's place and the last place of its continue construct.
pub(crate) struct Laid {
    pub(crate) blocks: Vec<Built>,
    pub(crate) places: Vec<(Segment, u32)>,
    pub(crate) loops: Vec<(u32, u32)>,
}

/// One inlined call of a function, or the entry point's own.
pub(crate) struct Instance {
    pub(crate) function: u32,
    /// Each local id's value, by its position past the function's start.
    pub(crate) values: Vec<Option<Val>>,
    /// Where its variables' private words start, and where its callees'
    /// may.
    pub(crate) private: u32,
    pub(crate) private_end: u32,
    /// Where its returns go and the register a returned value lands in.
    pub(crate) returns: Option<(Segment, Option<Reg>)>,
    /// Each block's first run's place once placed, whether it is reserved
    /// until the header that owns it releases it, and how many of the
    /// blocks it waits for are not yet placed.
    pub(crate) first: Vec<u32>,
    pub(crate) reserved: Vec<bool>,
    pub(crate) entering: Vec<u32>,
}

/// A block not yet placed.
const UNPLACED: u32 = u32::MAX;

/// Where the walk that orders a function's blocks stands with one.
#[derive(Copy, Clone, Eq, PartialEq)]
enum Walk {
    Unseen,
    Open,
    Done,
}

enum Task {
    /// Place a block once nothing holds it back: the header that owns it,
    /// releasing it, or a block it waits for, placing it last.
    Place {
        instance: u32,
        block: u32,
        owned: bool,
    },
    /// Place a block's run after a call, from the instruction past it.
    Resume { segment: Segment, from: u32 },
    /// A loop's continue construct ends here.
    LoopEnd { header: u32 },
}

impl Lowering<'_> {
    /// Index every function: where it lies, its parameters and its blocks.
    pub(crate) fn index(&mut self) -> Result<(), Fault> {
        let count = self.module().instructions().len();
        let mut at = 0;
        while at < count {
            let instruction = *self.at(position(at)?)?;
            if instruction.op() != Op::FUNCTION {
                at += 1;
                continue;
            }
            let start = position(at)?;
            let mut function = super::Function {
                start,
                end: start,
                parameters: Vec::new(),
                blocks: Vec::new(),
                reachable: Vec::new(),
                entering: Vec::new(),
                phis: Vec::new(),
                shaped: false,
            };
            let mut label = None;
            at += 1;
            while at < count {
                let inner = *self.at(position(at)?)?;
                let op = inner.op();
                if op == Op::FUNCTION_END {
                    break;
                }
                if op == Op::FUNCTION_PARAMETER {
                    let id = self.module().result(&inner).ok_or(Fault::Malformed)?;
                    push(&mut function.parameters, id, MAX_PROGRAM_INSTRUCTIONS)?;
                } else if op == Op::LABEL {
                    label = Some(position(at)?);
                } else if super::terminates(op) {
                    let first = label.take().ok_or(Fault::Malformed)?;
                    push(
                        &mut function.blocks,
                        (first, position(at)?),
                        MAX_PROGRAM_INSTRUCTIONS,
                    )?;
                }
                at += 1;
            }
            function.end = position(at)?;
            push(&mut self.functions, function, MAX_PROGRAM_INSTRUCTIONS)?;
            at += 1;
        }
        Ok(())
    }

    /// Which blocks a branch from the first reaches, and how many reached
    /// blocks branch to each other than round a loop: a block is laid out
    /// only once all of those are, so every edge but a back edge leads
    /// forward and a live range is a span of the layout.
    fn shape(&self, blocks: &[(u32, u32)]) -> Result<(Vec<bool>, Vec<u32>), Fault> {
        let count = blocks.len();
        // Each block's targets once each, the blocks' runs end to end.
        let mut starts = filled(count + 1, 0u32)?;
        let mut targets = Vec::new();
        let mut own = Vec::new();
        for (block, &(_, terminator)) in blocks.iter().enumerate() {
            own.clear();
            self.targets(blocks, terminator, &mut own)?;
            own.sort_unstable();
            own.dedup();
            for &target in &own {
                push(&mut targets, target, MAX_PROGRAM_INSTRUCTIONS)?;
            }
            if let Some(end) = starts.get_mut(block + 1) {
                *end = next_index(&targets)?;
            }
        }
        // A walk from the first block: in a reducible graph an edge to a
        // block still on the walk is a loop's back edge.
        let mut state = filled(count, Walk::Unseen)?;
        let mut entering = filled(count, 0u32)?;
        let mut walk: Vec<(u32, u32)> = Vec::new();
        if let Some(first) = state.first_mut() {
            *first = Walk::Open;
            push(&mut walk, (0, 0), count)?;
        }
        while let Some(top) = walk.last_mut() {
            let block = top.0 as usize;
            let (Some(&from), Some(&to)) = (starts.get(block), starts.get(block + 1)) else {
                return Err(Fault::Malformed);
            };
            let next = from + top.1;
            if next >= to {
                if let Some(done) = state.get_mut(block) {
                    *done = Walk::Done;
                }
                walk.pop();
                continue;
            }
            top.1 += 1;
            let target = targets
                .get(next as usize)
                .copied()
                .ok_or(Fault::Malformed)?;
            let seen = state.get_mut(target as usize).ok_or(Fault::Malformed)?;
            if *seen == Walk::Open {
                continue;
            }
            if let Some(entered) = entering.get_mut(target as usize) {
                *entered += 1;
            }
            if *seen == Walk::Unseen {
                *seen = Walk::Open;
                push(&mut walk, (target, 0), count)?;
            }
        }
        let mut reached = filled(count, false)?;
        for (reached, &seen) in reached.iter_mut().zip(&state) {
            *reached = seen != Walk::Unseen;
        }
        Ok((reached, entering))
    }

    /// The blocks a block's terminator at `terminator` branches to, pushed
    /// onto `out`.
    fn targets(
        &self,
        blocks: &[(u32, u32)],
        terminator: u32,
        out: &mut Vec<u32>,
    ) -> Result<(), Fault> {
        let instruction = *self.at(terminator)?;
        let switching = instruction.op() == Op::SWITCH;
        let labels = match instruction.op() {
            Op::BRANCH => 0..1,
            Op::BRANCH_CONDITIONAL => 1..3,
            Op::SWITCH => 1..self.operands(&instruction),
            _ => 0..0,
        };
        for index in labels {
            // A switch's literals sit between its targets.
            if switching && index > 1 && index % 2 == 0 {
                continue;
            }
            let target = block_index(self, blocks, self.id(&instruction, index)?)?;
            push(out, target, MAX_PROGRAM_INSTRUCTIONS)?;
        }
        Ok(())
    }

    /// Each phi of `function` as `(block, parent, phi, incoming)`, sorted so
    /// an edge's copies are one binary search away.
    /// Shape the function at `index` the first time it is instanced: which
    /// of its blocks a branch reaches, how many branch to each, and each
    /// phi's incoming value by the edge it arrives on.
    fn shape_once(&mut self, index: u32) -> Result<(), Fault> {
        let function = self.functions.get(index as usize).ok_or(Fault::Malformed)?;
        if function.shaped {
            return Ok(());
        }
        let (reachable, entering) = self.shape(&function.blocks)?;
        let phis = self.phis_of(&function.blocks)?;
        let function = self
            .functions
            .get_mut(index as usize)
            .ok_or(Fault::Malformed)?;
        (function.reachable, function.entering, function.phis) = (reachable, entering, phis);
        function.shaped = true;
        Ok(())
    }

    /// Each phi's incoming value among `blocks` as `(block, parent, phi,
    /// value)`, sorted.
    fn phis_of(&self, blocks: &[(u32, u32)]) -> Result<Vec<(u32, u32, u32, Id)>, Fault> {
        let mut phis = Vec::new();
        for (block, &(label, terminator)) in (0u32..).zip(blocks) {
            for at in label + 1..terminator {
                let instruction = *self.at(at)?;
                match instruction.op() {
                    Op::PHI => {}
                    Op::LINE | Op::NO_LINE => continue,
                    _ => break,
                }
                let mut pair = 2;
                while pair + 1 < self.operands(&instruction) {
                    let incoming = self.id(&instruction, pair)?;
                    let parent = self.id(&instruction, pair + 1)?;
                    let parent = block_index(self, blocks, parent)?;
                    push(
                        &mut phis,
                        (block, parent, at, incoming),
                        MAX_PROGRAM_INSTRUCTIONS,
                    )?;
                    pair += 2;
                }
            }
        }
        phis.sort_unstable_by_key(|&(block, parent, at, _)| (block, parent, at));
        Ok(phis)
    }

    /// Declare every global variable the entry point reaches: built-ins as
    /// the memory a tile gives them, bindings by the entry point's own list,
    /// push constants, private and workgroup variables. Another entry
    /// point's are left alone, and constants are laid out when first used.
    pub(crate) fn declare_globals(&mut self) -> Result<(), Fault> {
        let first_function = self
            .functions
            .first()
            .map_or(self.module().instructions().len(), |f| f.start as usize);
        self.globals
            .try_reserve_exact(first_function)
            .map_err(|_| Fault::OutOfMemory)?;
        self.globals.resize(first_function, None);
        for binding in self.entry.bindings() {
            push(
                &mut self.program.bindings,
                crate::software::shader::ir::Binding {
                    set: binding.set,
                    number: binding.binding,
                    count: binding.count,
                    first: 0,
                },
                MAX_PROGRAM_INSTRUCTIONS,
            )?;
        }
        let reached = self.entry.globals();
        for at in 0..first_function {
            let instruction = *self.at(position(at)?)?;
            if instruction.op() != Op::VARIABLE {
                continue;
            }
            let id = self.module().result(&instruction).ok_or(Fault::Malformed)?;
            if reached.binary_search(&id).is_err() {
                continue;
            }
            let pointer = self.result_type(&instruction)?;
            let Type::Pointer { class, pointee } = self.ty(pointer)? else {
                return Err(Fault::Malformed);
            };
            let val = self.global_variable(id, class, pointee)?;
            if let Some(slot) = self.globals.get_mut(at) {
                *slot = Some(val);
            }
        }
        Ok(())
    }

    /// Lay out the entry point's blocks, lowering each as it is placed.
    pub(crate) fn lay_out(&mut self) -> Result<Laid, Fault> {
        let entry = self.function_at(self.entry.function())?;
        let private = self.program.private_words;
        let mut instances = Vec::new();
        self.instance(&mut instances, entry, private, None)?;
        let mut laid = Laid {
            blocks: Vec::new(),
            places: Vec::new(),
            loops: Vec::new(),
        };
        let mut tasks = Vec::new();
        push(
            &mut tasks,
            Task::Place {
                instance: 0,
                block: 0,
                owned: true,
            },
            MAX_BLOCKS,
        )?;
        while let Some(task) = tasks.pop() {
            match task {
                Task::Place {
                    instance,
                    block,
                    owned,
                } => {
                    let state = instances
                        .get_mut(instance as usize)
                        .ok_or(Fault::Malformed)?;
                    let at = block as usize;
                    if owned {
                        if let Some(reserved) = state.reserved.get_mut(at) {
                            *reserved = false;
                        }
                    }
                    let placed = state.first.get(at).is_none_or(|&p| p != UNPLACED);
                    let reserved = state.reserved.get(at).copied().unwrap_or(true);
                    let waiting = state.entering.get(at).is_none_or(|&n| n != 0);
                    let reachable = self
                        .functions
                        .get(state.function as usize)
                        .and_then(|f| f.reachable.get(at))
                        .copied()
                        .unwrap_or(false);
                    if placed || reserved || waiting || !reachable {
                        continue;
                    }
                    let (label, _) = self.block_bounds(&instances, instance, block)?;
                    let segment = Segment {
                        instance,
                        block,
                        run: 0,
                    };
                    self.run(&mut instances, &mut laid, &mut tasks, segment, label + 1)?;
                }
                Task::Resume { segment, from } => {
                    self.run(&mut instances, &mut laid, &mut tasks, segment, from)?;
                }
                Task::LoopEnd { header } => {
                    let end = next_index(&laid.blocks)?
                        .checked_sub(1)
                        .ok_or(Fault::Malformed)?;
                    push(&mut laid.loops, (header, end), MAX_BLOCKS)?;
                }
            }
        }
        let most = instances
            .iter()
            .fold(self.program.private_words, |most, i| {
                most.max(i.private_end)
            });
        self.program.private_words = most;
        Ok(laid)
    }

    /// The function whose `OpFunction` defines `id`.
    pub(crate) fn function_at(&self, id: Id) -> Result<u32, Fault> {
        let at = position(self.module().position(id).ok_or(Fault::Malformed)?)?;
        let index = self
            .functions
            .binary_search_by_key(&at, |f| f.start)
            .map_err(|_| Fault::Malformed)?;
        u32::try_from(index).map_err(|_| Fault::Budget)
    }

    /// An instance of function `function`, its variables from private word
    /// `private`, returning to `returns`.
    fn instance(
        &mut self,
        instances: &mut Vec<Instance>,
        function: u32,
        private: u32,
        returns: Option<(Segment, Option<Reg>)>,
    ) -> Result<u32, Fault> {
        self.shape_once(function)?;
        let f = self
            .functions
            .get(function as usize)
            .ok_or(Fault::Malformed)?;
        let span = (f.end - f.start) as usize;
        let blocks = f.blocks.len();
        self.instanced = self
            .instanced
            .checked_add(span)
            .filter(|&total| total <= MAX_PROGRAM_INSTRUCTIONS)
            .ok_or(Fault::Budget)?;
        let values = filled(span, None)?;
        let first = filled(blocks, UNPLACED)?;
        let reserved = filled(blocks, false)?;
        let mut entering = Vec::new();
        entering
            .try_reserve_exact(blocks)
            .map_err(|_| Fault::OutOfMemory)?;
        entering.extend_from_slice(&f.entering);
        let index = next_index(instances)?;
        push(
            instances,
            Instance {
                function,
                values,
                private,
                private_end: private,
                returns,
                first,
                reserved,
                entering,
            },
            MAX_INSTANCES,
        )?;
        self.declare_locals(instances, index)?;
        Ok(index)
    }

    /// A block's label position and terminator position.
    fn block_bounds(
        &self,
        instances: &[Instance],
        instance: u32,
        block: u32,
    ) -> Result<(u32, u32), Fault> {
        let function = instances
            .get(instance as usize)
            .ok_or(Fault::Malformed)?
            .function;
        self.functions
            .get(function as usize)
            .and_then(|f| f.blocks.get(block as usize))
            .copied()
            .ok_or(Fault::Malformed)
    }

    /// The block of `instance`'s function `label` begins.
    fn block_of(&self, instances: &[Instance], instance: u32, label: Id) -> Result<u32, Fault> {
        let function = instances
            .get(instance as usize)
            .ok_or(Fault::Malformed)?
            .function;
        let blocks = &self
            .functions
            .get(function as usize)
            .ok_or(Fault::Malformed)?
            .blocks;
        block_index(self, blocks, label)
    }

    /// Lower one run of a block, from instruction `from` to the next call,
    /// barrier or its terminator, and schedule what follows it.
    fn run(
        &mut self,
        instances: &mut Vec<Instance>,
        laid: &mut Laid,
        tasks: &mut Vec<Task>,
        segment: Segment,
        from: u32,
    ) -> Result<(), Fault> {
        let place = next_index(&laid.blocks)?;
        push(&mut laid.places, (segment, place), MAX_BLOCKS)?;
        if segment.run == 0 {
            if let Some(first) = instances
                .get_mut(segment.instance as usize)
                .and_then(|i| i.first.get_mut(segment.block as usize))
            {
                *first = place;
            }
        }
        let mut built = Built {
            insts: Vec::new(),
            term: Term::Exit,
        };
        if segment
            == (Segment {
                instance: 0,
                block: 0,
                run: 0,
            })
        {
            self.initialize_entry(instances, &mut built)?;
        }
        let (_, terminator) = self.block_bounds(instances, segment.instance, segment.block)?;
        let mut at = from;
        while at < terminator {
            let instruction = *self.at(at)?;
            self.count(Count::Operands, self.operands(&instruction))?;
            let op = instruction.op();
            let across = op == Op::CONTROL_BARRIER && self.barrier_across_tiles(&instruction)?;
            if op == Op::PHI {
                self.declare_phi(instances, segment.instance, at)?;
            } else if op == Op::FUNCTION_CALL {
                let after = Segment {
                    run: segment.run + 1,
                    ..segment
                };
                let callee = self.call(instances, &mut built, segment.instance, at, after)?;
                built.term = Term::Branch(Edge {
                    target: Segment {
                        instance: callee,
                        block: 0,
                        run: 0,
                    },
                    moves: Vec::new(),
                });
                push(&mut laid.blocks, built, MAX_BLOCKS)?;
                push(
                    tasks,
                    Task::Resume {
                        segment: after,
                        from: at + 1,
                    },
                    MAX_BLOCKS,
                )?;
                push(
                    tasks,
                    Task::Place {
                        instance: callee,
                        block: 0,
                        owned: true,
                    },
                    MAX_BLOCKS,
                )?;
                return Ok(());
            } else if across {
                self.barrier_fence(&mut built, &instruction)?;
                let after = Segment {
                    run: segment.run + 1,
                    ..segment
                };
                built.term = Term::Barrier(Edge {
                    target: after,
                    moves: Vec::new(),
                });
                push(&mut laid.blocks, built, MAX_BLOCKS)?;
                push(
                    tasks,
                    Task::Resume {
                        segment: after,
                        from: at + 1,
                    },
                    MAX_BLOCKS,
                )?;
                return Ok(());
            } else {
                self.select(instances, &mut built, segment.instance, at)?;
            }
            at += 1;
        }
        let ended = *self.at(terminator)?;
        self.count(Count::Operands, self.operands(&ended))?;
        let (term, successors) = self.terminator(instances, segment, terminator)?;
        built.term = term;
        push(&mut laid.blocks, built, MAX_BLOCKS)?;
        self.schedule(instances, tasks, segment, terminator, &successors)
    }

    /// Whether a control barrier holds the workgroup's tiles together, its
    /// execution scope reaching past a subgroup.
    fn barrier_across_tiles(&self, instruction: &Instruction) -> Result<bool, Fault> {
        let scope = self.constant_u32(self.id(instruction, 0)?)?;
        Ok(scope != Scope::SUBGROUP.0 && scope != Scope::INVOCATION.0)
    }

    /// The value of a constant 32-bit integer the module names.
    pub(crate) fn constant_u32(&self, id: Id) -> Result<u32, Fault> {
        let constant = self
            .specialized
            .constant(self.admitted, id)
            .ok_or(Fault::Malformed)?;
        match self.specialized.value(constant).ok_or(Fault::Malformed)? {
            tairix_spirv::Constant::Scalar(bits) => {
                u32::try_from(bits & 0xffff_ffff).map_err(|_| Fault::Malformed)
            }
            tairix_spirv::Constant::Null => Ok(0),
            tairix_spirv::Constant::Composite(_) => Err(Fault::Malformed),
        }
    }

    /// Schedule what follows a block's last run: a header's construct, its
    /// continue construct and its merge, or the block's successors.
    fn schedule(
        &mut self,
        instances: &mut [Instance],
        tasks: &mut Vec<Task>,
        segment: Segment,
        terminator: u32,
        successors: &[u32],
    ) -> Result<(), Fault> {
        let instance = segment.instance;
        let merge = *self.at(terminator.checked_sub(1).ok_or(Fault::Malformed)?)?;
        let reserve = |instances: &mut [Instance], block: u32| {
            if let Some(flag) = instances
                .get_mut(instance as usize)
                .and_then(|i| i.reserved.get_mut(block as usize))
            {
                *flag = true;
            }
        };
        let place_task = |block: u32, owned: bool| Task::Place {
            instance,
            block,
            owned,
        };
        if let Some(state) = instances.get_mut(instance as usize) {
            for &successor in successors {
                let at = successor as usize;
                if state.first.get(at) == Some(&UNPLACED) {
                    if let Some(waiting) = state.entering.get_mut(at) {
                        *waiting = waiting.saturating_sub(1);
                    }
                }
            }
        }
        match merge.op() {
            Op::LOOP_MERGE => {
                let merge_block = self.block_of(instances, instance, self.id(&merge, 0)?)?;
                let continue_block = self.block_of(instances, instance, self.id(&merge, 1)?)?;
                reserve(instances, merge_block);
                reserve(instances, continue_block);
                let header = instances
                    .get(instance as usize)
                    .and_then(|i| i.first.get(segment.block as usize))
                    .copied()
                    .filter(|&p| p != UNPLACED)
                    .ok_or(Fault::Malformed)?;
                push(tasks, place_task(merge_block, true), MAX_BLOCKS)?;
                push(tasks, Task::LoopEnd { header }, MAX_BLOCKS)?;
                push(tasks, place_task(continue_block, true), MAX_BLOCKS)?;
                for &successor in successors.iter().rev() {
                    push(tasks, place_task(successor, false), MAX_BLOCKS)?;
                }
            }
            Op::SELECTION_MERGE => {
                let merge_block = self.block_of(instances, instance, self.id(&merge, 0)?)?;
                reserve(instances, merge_block);
                push(tasks, place_task(merge_block, true), MAX_BLOCKS)?;
                for &successor in successors.iter().rev() {
                    if successor != merge_block {
                        push(tasks, place_task(successor, false), MAX_BLOCKS)?;
                    }
                }
            }
            _ => {
                for &successor in successors.iter().rev() {
                    push(tasks, place_task(successor, false), MAX_BLOCKS)?;
                }
            }
        }
        Ok(())
    }

    /// Inline a call: its arguments bound to the callee's parameters, its
    /// variables placed past the caller's, and its returns sent to `after`.
    fn call(
        &mut self,
        instances: &mut Vec<Instance>,
        built: &mut Built,
        caller: u32,
        at: u32,
        after: Segment,
    ) -> Result<u32, Fault> {
        let instruction = *self.at(at)?;
        let function = self.function_at(self.id(&instruction, 2)?)?;
        let returns = self.result_type(&instruction)?;
        let words = self.words(returns)?;
        // A result of no words still names a register, which nothing reads.
        let result = if matches!(self.ty(returns)?, Type::Void) {
            None
        } else {
            let reg = self.fresh(words, false)?;
            let id = self.module().result(&instruction).ok_or(Fault::Malformed)?;
            self.define(instances, caller, id, Val::Data { reg, ty: returns })?;
            Some(reg)
        };
        let private = instances
            .get(caller as usize)
            .ok_or(Fault::Malformed)?
            .private_end;
        let instance = self.instance(instances, function, private, Some((after, result)))?;
        let count = self
            .functions
            .get(function as usize)
            .ok_or(Fault::Malformed)?
            .parameters
            .len();
        for index in 0..count {
            let parameter = self
                .functions
                .get(function as usize)
                .and_then(|f| f.parameters.get(index))
                .copied()
                .ok_or(Fault::Malformed)?;
            let argument = self.id(&instruction, 3 + index)?;
            let value = self.value(instances, caller, argument)?;
            self.define(instances, instance, parameter, value)?;
        }
        // The callee's variables live only while it runs, so a later call
        // from the same caller reuses their words.
        self.initialize_locals(instances, instance, built)?;
        Ok(instance)
    }

    /// The value `id` holds in `instance`: a local's, or a global's, its
    /// definition found once for either.
    pub(crate) fn value(
        &mut self,
        instances: &[Instance],
        instance: u32,
        id: Id,
    ) -> Result<Val, Fault> {
        let at = self.module().position(id).ok_or(Fault::Malformed)?;
        if let Some(local) = self.local_at(instances, instance, at)? {
            return instances
                .get(instance as usize)
                .and_then(|i| i.values.get(local))
                .copied()
                .flatten()
                .ok_or(Fault::Malformed);
        }
        self.global_at(id, at)?.ok_or(Fault::Malformed)
    }

    /// Where a local id's value is kept in its instance, or `None` for an
    /// id the function does not define.
    fn local_slot(
        &self,
        instances: &[Instance],
        instance: u32,
        id: Id,
    ) -> Result<Option<usize>, Fault> {
        match self.module().position(id) {
            Some(at) => self.local_at(instances, instance, at),
            None => Ok(None),
        }
    }

    /// Where the id defined at position `at` keeps its value in `instance`,
    /// or `None` where the instance's function does not define it.
    fn local_at(
        &self,
        instances: &[Instance],
        instance: u32,
        at: usize,
    ) -> Result<Option<usize>, Fault> {
        let function = instances
            .get(instance as usize)
            .ok_or(Fault::Malformed)?
            .function;
        let f = self
            .functions
            .get(function as usize)
            .ok_or(Fault::Malformed)?;
        let Ok(at) = u32::try_from(at) else {
            return Ok(None);
        };
        Ok((f.start..f.end)
            .contains(&at)
            .then(|| (at - f.start) as usize))
    }

    /// Give local `id` of `instance` its value.
    pub(crate) fn define(
        &self,
        instances: &mut [Instance],
        instance: u32,
        id: Id,
        val: Val,
    ) -> Result<(), Fault> {
        let slot = self
            .local_slot(instances, instance, id)?
            .ok_or(Fault::Malformed)?;
        let value = instances
            .get_mut(instance as usize)
            .and_then(|i| i.values.get_mut(slot))
            .ok_or(Fault::Malformed)?;
        *value = Some(val);
        Ok(())
    }

    /// A phi's register, made on first sight: by the first edge into its
    /// block or by the block itself.
    fn declare_phi(
        &mut self,
        instances: &mut [Instance],
        instance: u32,
        at: u32,
    ) -> Result<Reg, Fault> {
        let instruction = *self.at(at)?;
        let id = self.module().result(&instruction).ok_or(Fault::Malformed)?;
        let slot = self
            .local_slot(instances, instance, id)?
            .ok_or(Fault::Malformed)?;
        if let Some(Some(val)) = instances
            .get(instance as usize)
            .and_then(|i| i.values.get(slot))
        {
            return Ok(val.reg());
        }
        let ty = self.result_type(&instruction)?;
        let words = self.words(ty)?;
        let reg = self.fresh(words, false)?;
        self.define(instances, instance, id, Val::Data { reg, ty })?;
        Ok(reg)
    }

    /// The copies an edge from block `from` into block `to` makes for
    /// `to`'s phis.
    fn moves(
        &mut self,
        instances: &mut [Instance],
        instance: u32,
        from: u32,
        to: u32,
    ) -> Result<Vec<Move>, Fault> {
        let function = instances
            .get(instance as usize)
            .ok_or(Fault::Malformed)?
            .function;
        let phis = &self
            .functions
            .get(function as usize)
            .ok_or(Fault::Malformed)?
            .phis;
        let first = phis.partition_point(|&(block, parent, _, _)| (block, parent) < (to, from));
        let last = phis.partition_point(|&(block, parent, _, _)| (block, parent) <= (to, from));
        self.count(Count::Operands, last - first)?;
        let mut moves = Vec::new();
        for entry in first..last {
            let (_, _, at, incoming) = self
                .functions
                .get(function as usize)
                .and_then(|f| f.phis.get(entry))
                .copied()
                .ok_or(Fault::Malformed)?;
            let dst = self.declare_phi(instances, instance, at)?;
            let ty = self.result_type(self.at(at)?)?;
            let words = self.words(ty)?;
            let src = self.value(instances, instance, incoming)?.reg();
            if words > 0 {
                self.count(Count::Copies, 1)?;
                push(
                    &mut moves,
                    Move { dst, src, words },
                    MAX_PROGRAM_INSTRUCTIONS,
                )?;
            }
        }
        self.sequence(moves)
    }

    /// An edge's copies ordered so none overwrites a value another still
    /// reads, each cycle broken through the scratch registers once every
    /// copy that could wait has been made.
    fn sequence(&mut self, mut moves: Vec<Move>) -> Result<Vec<Move>, Fault> {
        if moves.len() < 2 {
            return Ok(moves);
        }
        let mut by_dst = Vec::new();
        by_dst
            .try_reserve_exact(moves.len())
            .map_err(|_| Fault::OutOfMemory)?;
        by_dst.extend(
            moves
                .iter()
                .enumerate()
                .map(|(i, m)| (dst_key(m.dst), m.words.max(1), i)),
        );
        by_dst.sort_unstable();
        // The move writing the words `reg` starts at: a source may be a part
        // read where it lies inside another move's destination.
        let writer = |reg: Reg| {
            let (uniform, slot) = dst_key(reg);
            let after = by_dst.partition_point(|&(key, _, _)| key <= (uniform, slot));
            let &((class, first), words, i) = by_dst.get(after.checked_sub(1)?)?;
            (class == uniform && slot - first < words).then_some(i)
        };
        // How many other copies still read each copy's destination.
        let mut readers = Vec::new();
        readers
            .try_reserve_exact(moves.len())
            .map_err(|_| Fault::OutOfMemory)?;
        readers.resize(moves.len(), 0u32);
        for (i, m) in moves.iter().enumerate() {
            if let Some(count) = writer(m.src)
                .filter(|&w| w != i)
                .and_then(|w| readers.get_mut(w))
            {
                *count += 1;
            }
        }
        let mut done = Vec::new();
        done.try_reserve_exact(moves.len())
            .map_err(|_| Fault::OutOfMemory)?;
        done.resize(moves.len(), false);
        let mut ready = Vec::new();
        for (i, &count) in readers.iter().enumerate() {
            if count == 0 {
                push(&mut ready, i, MAX_PROGRAM_INSTRUCTIONS)?;
            }
        }
        let mut ordered = Vec::new();
        ordered
            .try_reserve_exact(moves.len())
            .map_err(|_| Fault::OutOfMemory)?;
        let mut remaining = moves.len();
        let mut next_cycle = 0;
        while remaining > 0 {
            let (released, broken) = if let Some(i) = ready.pop() {
                if done.get(i) != Some(&false) {
                    continue;
                }
                let m = *moves.get(i).ok_or(Fault::Malformed)?;
                push(&mut ordered, m, MAX_PROGRAM_INSTRUCTIONS)?;
                if let Some(flag) = done.get_mut(i) {
                    *flag = true;
                }
                remaining -= 1;
                (writer(m.src).filter(|&w| w != i), None)
            } else {
                while done.get(next_cycle) == Some(&true) {
                    next_cycle += 1;
                }
                let words = moves.get(next_cycle).ok_or(Fault::Malformed)?.words;
                let scratch = self.scratch(words);
                let m = moves.get_mut(next_cycle).ok_or(Fault::Malformed)?;
                let blocked = writer(m.src).filter(|&w| w != next_cycle);
                let saved = Move {
                    dst: scratch,
                    src: m.src,
                    words,
                };
                m.src = scratch;
                push(&mut ordered, saved, MAX_PROGRAM_INSTRUCTIONS)?;
                (blocked, Some(next_cycle))
            };
            if let Some(w) = released {
                if let Some(count) = readers.get_mut(w) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        push(&mut ready, w, MAX_PROGRAM_INSTRUCTIONS)?;
                    }
                }
            }
            if let Some(broken) = broken {
                if readers.get(broken) == Some(&0) {
                    push(&mut ready, broken, MAX_PROGRAM_INSTRUCTIONS)?;
                }
            }
        }
        Ok(ordered)
    }

    /// The scratch planes, at least `words` long.
    fn scratch(&mut self, words: u32) -> Reg {
        self.scratch_words = self.scratch_words.max(words);
        self.program.scratch
    }

    /// Lower a block's terminator: its edges with their copies, and the
    /// blocks it leads to within its instance.
    #[expect(clippy::too_many_lines, reason = "one arm per terminator")]
    fn terminator(
        &mut self,
        instances: &mut [Instance],
        segment: Segment,
        at: u32,
    ) -> Result<(Term, Vec<u32>), Fault> {
        let instruction = *self.at(at)?;
        let instance = segment.instance;
        let mut successors = Vec::new();
        let into =
            |this: &mut Self, instances: &mut [Instance], block: u32, successors: &mut Vec<u32>| {
                push(successors, block, MAX_BLOCKS)?;
                let moves = this.moves(instances, instance, segment.block, block)?;
                Ok::<Edge, Fault>(Edge {
                    target: Segment {
                        instance,
                        block,
                        run: 0,
                    },
                    moves,
                })
            };
        let edge =
            |this: &mut Self, instances: &mut [Instance], label: Id, successors: &mut Vec<u32>| {
                let block = this.block_of(instances, instance, label)?;
                into(this, instances, block, successors)
            };
        let term = match instruction.op() {
            Op::BRANCH => Term::Branch(edge(
                self,
                instances,
                self.id(&instruction, 0)?,
                &mut successors,
            )?),
            Op::BRANCH_CONDITIONAL => {
                let cond = self
                    .value(instances, instance, self.id(&instruction, 0)?)?
                    .reg();
                let then = edge(self, instances, self.id(&instruction, 1)?, &mut successors)?;
                let other = edge(self, instances, self.id(&instruction, 2)?, &mut successors)?;
                Term::Conditional { cond, then, other }
            }
            Op::SWITCH => {
                let Val::Data { reg, ty } =
                    self.value(instances, instance, self.id(&instruction, 0)?)?
                else {
                    return Err(Fault::Malformed);
                };
                let wide = self.words(ty)? == 2;
                let Type::Scalar(Scalar::Int { width, .. }) = self.ty(ty)? else {
                    return Err(Fault::Malformed);
                };
                // A narrow selector is held zero-extended; its literals are
                // written sign-extended where it is signed.
                let held = mask(width);
                let default = self.block_of(instances, instance, self.id(&instruction, 1)?)?;
                let mut labels = Vec::new();
                let mut index = 2;
                self.count(
                    Count::Cases,
                    self.operands(&instruction).saturating_sub(2) / 2,
                )?;
                while index + 1 < self.operands(&instruction) {
                    let literal = match self.operand(&instruction, index)? {
                        Value::Number(n) => n,
                        Value::Word(w) => u64::from(w),
                        _ => return Err(Fault::Malformed),
                    };
                    let block =
                        self.block_of(instances, instance, self.id(&instruction, index + 1)?)?;
                    push(
                        &mut labels,
                        (literal & held, block),
                        MAX_PROGRAM_INSTRUCTIONS,
                    )?;
                    index += 2;
                }
                // One edge for each target, in the order the switch first
                // names it, however many labels share it.
                let mut named = Vec::new();
                push(&mut named, (default, 0u32), MAX_PROGRAM_INSTRUCTIONS)?;
                for (order, &(_, block)) in (1u32..).zip(&labels) {
                    push(&mut named, (block, order), MAX_PROGRAM_INSTRUCTIONS)?;
                }
                named.sort_unstable();
                named.dedup_by_key(|&mut (block, _)| block);
                named.sort_unstable_by_key(|&(_, first)| first);
                let mut edges = Vec::new();
                let mut edge_of = Vec::new();
                edge_of
                    .try_reserve_exact(named.len())
                    .map_err(|_| Fault::OutOfMemory)?;
                for (edge, &(block, _)) in (0u32..).zip(&named) {
                    let built = into(self, instances, block, &mut successors)?;
                    push(&mut edges, built, MAX_BLOCKS)?;
                    edge_of.push((block, edge));
                }
                edge_of.sort_unstable();
                let find = |block: u32| {
                    edge_of
                        .binary_search_by_key(&block, |&(b, _)| b)
                        .ok()
                        .and_then(|at| edge_of.get(at))
                        .map(|&(_, edge)| edge)
                        .ok_or(Fault::Malformed)
                };
                let mut cases = Vec::new();
                cases
                    .try_reserve_exact(labels.len())
                    .map_err(|_| Fault::OutOfMemory)?;
                for &(literal, block) in &labels {
                    cases.push((literal, find(block)?));
                }
                Term::Switch {
                    selector: reg,
                    wide,
                    edges,
                    cases,
                    default: find(default)?,
                }
            }
            Op::RETURN | Op::RETURN_VALUE => {
                let returns = instances
                    .get(instance as usize)
                    .ok_or(Fault::Malformed)?
                    .returns;
                match returns {
                    None => Term::Exit,
                    Some((after, result)) => {
                        let mut moves = Vec::new();
                        if let (Some(dst), Op::RETURN_VALUE) = (result, instruction.op()) {
                            let Val::Data { reg, ty } =
                                self.value(instances, instance, self.id(&instruction, 0)?)?
                            else {
                                return Err(Fault::Malformed);
                            };
                            let words = self.words(ty)?;
                            if words > 0 {
                                self.count(Count::Copies, 1)?;
                                push(
                                    &mut moves,
                                    Move {
                                        dst,
                                        src: reg,
                                        words,
                                    },
                                    1,
                                )?;
                            }
                        }
                        Term::Branch(Edge {
                            target: after,
                            moves,
                        })
                    }
                }
            }
            Op::UNREACHABLE => Term::Exit,
            Op::KILL | Op::TERMINATE_INVOCATION => Term::Kill,
            _ => return Err(Fault::Malformed),
        };
        // A conditional naming one block twice reaches it once.
        successors.dedup();
        Ok((term, successors))
    }

    /// Declare an instance's own variables in its private words, past its
    /// caller's.
    fn declare_locals(&mut self, instances: &mut [Instance], instance: u32) -> Result<(), Fault> {
        let state = instances.get(instance as usize).ok_or(Fault::Malformed)?;
        let function = self
            .functions
            .get(state.function as usize)
            .ok_or(Fault::Malformed)?;
        let &(label, terminator) = function.blocks.first().ok_or(Fault::Malformed)?;
        let mut offset = state.private;
        for at in label + 1..terminator {
            let instruction = *self.at(at)?;
            if instruction.op() != Op::VARIABLE {
                continue;
            }
            let id = self.module().result(&instruction).ok_or(Fault::Malformed)?;
            let Type::Pointer { pointee, class } = self.ty(self.result_type(&instruction)?)? else {
                return Err(Fault::Malformed);
            };
            if class != StorageClass::FUNCTION {
                return Err(Fault::Malformed);
            }
            let words = self.words(pointee)?;
            let pointer = self.zeros(2)?;
            let val = memory::private_pointer(offset, words, pointee, pointer);
            self.define(instances, instance, id, val)?;
            offset = offset.checked_add(words).ok_or(Fault::Budget)?;
        }
        if let Some(state) = instances.get_mut(instance as usize) {
            state.private_end = offset;
        }
        Ok(())
    }

    /// Zero a callee's variables and give each its initializer, as its call
    /// begins.
    fn initialize_locals(
        &mut self,
        instances: &mut [Instance],
        instance: u32,
        built: &mut Built,
    ) -> Result<(), Fault> {
        let state = instances.get(instance as usize).ok_or(Fault::Malformed)?;
        let function = self
            .functions
            .get(state.function as usize)
            .ok_or(Fault::Malformed)?;
        let &(label, terminator) = function.blocks.first().ok_or(Fault::Malformed)?;
        for at in label + 1..terminator {
            let instruction = *self.at(at)?;
            if instruction.op() != Op::VARIABLE {
                continue;
            }
            let id = self.module().result(&instruction).ok_or(Fault::Malformed)?;
            let pointer = self.value(instances, instance, id)?;
            let initial = if self.operands(&instruction) > 3 {
                Some(self.value(instances, instance, self.id(&instruction, 3)?)?)
            } else {
                None
            };
            self.initialize(&mut built.insts, pointer, initial)?;
        }
        Ok(())
    }

    /// Give the entry point's variables with initializers, and the private
    /// globals with them, their values: the tile's private words start
    /// zeroed.
    fn initialize_entry(
        &mut self,
        instances: &mut [Instance],
        built: &mut Built,
    ) -> Result<(), Fault> {
        let first_function = self.globals.len();
        for at in 0..first_function {
            let instruction = *self.at(position(at)?)?;
            if instruction.op() != Op::VARIABLE || self.operands(&instruction) <= 3 {
                continue;
            }
            // A variable the entry point does not reach was never declared.
            let Some(pointer) = self.globals.get(at).copied().flatten() else {
                continue;
            };
            let initial = self.value(instances, 0, self.id(&instruction, 3)?)?;
            self.initialize(&mut built.insts, pointer, Some(initial))?;
        }
        let state = instances.first().ok_or(Fault::Malformed)?;
        let function = self
            .functions
            .get(state.function as usize)
            .ok_or(Fault::Malformed)?;
        let &(label, terminator) = function.blocks.first().ok_or(Fault::Malformed)?;
        for at in label + 1..terminator {
            let instruction = *self.at(at)?;
            if instruction.op() != Op::VARIABLE || self.operands(&instruction) <= 3 {
                continue;
            }
            let id = self.module().result(&instruction).ok_or(Fault::Malformed)?;
            let pointer = self.value(instances, 0, id)?;
            let initial = self.value(instances, 0, self.id(&instruction, 3)?)?;
            self.initialize(&mut built.insts, pointer, Some(initial))?;
        }
        Ok(())
    }
}

/// The block among `blocks` `label` begins.
fn block_index(lowering: &Lowering<'_>, blocks: &[(u32, u32)], label: Id) -> Result<u32, Fault> {
    let at = position(lowering.module().position(label).ok_or(Fault::Malformed)?)?;
    let index = blocks
        .binary_search_by_key(&at, |&(first, _)| first)
        .map_err(|_| Fault::Malformed)?;
    u32::try_from(index).map_err(|_| Fault::Budget)
}

fn position(at: usize) -> Result<u32, Fault> {
    u32::try_from(at).map_err(|_| Fault::Budget)
}

/// A register's order among destinations.
const fn dst_key(reg: Reg) -> (bool, u32) {
    (reg.is_uniform(), reg.slot())
}

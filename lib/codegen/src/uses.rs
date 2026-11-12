//! Which values the code needs, which reads of a constant a target encodes
//! in the instruction rather than a register, and which branches test their
//! comparison directly. An instruction with no effect whose value nothing
//! needs is never lowered, and a constant read only where the target folds it
//! is never materialized; so neither holds a register. Loads, stores and
//! calls are always kept: a load may trap. A branch whose condition is a
//! comparison made in its own block reads the comparison's operands at the
//! branch, so the comparison is made only where its value is needed too;
//! on a target that tests one directly, so does such a select.

use alloc::vec;
use alloc::vec::Vec;

use crate::ir::{Cond, Function, Inst, Terminator, Type, Value};

/// What a target encodes as an immediate.
#[derive(Clone, Copy)]
pub(crate) struct Folding {
    /// Whether operand `operand` of an instruction, a constant of `ty`
    /// holding `imm`, is.
    pub(crate) inst: fn(inst: &Inst, operand: usize, ty: Type, imm: u64) -> bool,
    /// Whether the second operand of a comparison a branch is made on, of
    /// `ty` holding `imm`, is.
    pub(crate) branch: fn(cond: Cond, ty: Type, imm: u64) -> bool,
    /// Whether a select tests a comparison made in its own block directly,
    /// that comparison's second operand folded as a branch's is.
    pub(crate) select: bool,
}

/// Where a value is defined.
#[derive(Clone, Copy)]
enum Def {
    None,
    Param { block: usize, index: usize },
    Inst { block: usize, index: usize },
}

/// A comparison a branch or a select is made on: `a cond b`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Compare {
    pub(crate) cond: Cond,
    pub(crate) a: Value,
    pub(crate) b: Value,
}

/// The needed values of a function, for one target.
pub(crate) struct Uses {
    consts: Vec<Option<u64>>,
    live: Vec<bool>,
    /// Each block's branch's comparison, where it tests one directly.
    fused: Vec<Option<Compare>>,
    /// Each select's comparison, by the select's value, where it tests one
    /// directly.
    selects: Vec<Option<Compare>>,
    folding: Folding,
}

/// Whether `inst` does nothing but define its value.
const fn pure(inst: &Inst) -> bool {
    !matches!(
        inst,
        Inst::Load { .. } | Inst::Store { .. } | Inst::Call { .. }
    )
}

impl Uses {
    /// The needs of `func`'s blocks in `order`, those the entry reaches, as
    /// a target folding by `folding` has them.
    pub(crate) fn new(func: &Function, order: &[usize], folding: Folding) -> Self {
        let count = func.value_count();
        let mut consts = vec![None; count];
        let mut defs = vec![Def::None; count];
        for (block, data) in func.blocks().iter().enumerate() {
            for (index, &param) in data.params.iter().enumerate() {
                defs[param.index()] = Def::Param { block, index };
            }
            for (index, (defined, inst)) in data.insts.iter().enumerate() {
                if let Some(value) = *defined {
                    defs[value.index()] = Def::Inst { block, index };
                    if let Inst::Const { imm, .. } = *inst {
                        consts[value.index()] = Some(imm);
                    }
                }
            }
        }
        // Each block's incoming edges: the predecessor, and which of its
        // successors the block is.
        let mut incoming: Vec<Vec<(usize, usize)>> = vec![Vec::new(); func.blocks().len()];
        let mut fused = vec![None; func.blocks().len()];
        let mut selects = vec![None; count];
        for &block in order {
            let data = &func.blocks()[block];
            // The comparison `cond` is, where it is made in this block.
            let made_here = |cond: Value| match defs.get(cond.index()) {
                Some(&Def::Inst { block: at, index }) if at == block => match data.insts.get(index)
                {
                    Some(&(_, Inst::Icmp { cond, a, b })) => Some(Compare { cond, a, b }),
                    _ => None,
                },
                _ => None,
            };
            for (which, (to, _)) in data.term.successors().enumerate() {
                incoming[to.index()].push((block, which));
            }
            if let Terminator::Branch { cond, .. } = data.term {
                fused[block] = made_here(cond);
            }
            if folding.select {
                for &(defined, ref inst) in &data.insts {
                    if let (Some(value), &Inst::Select { cond, .. }) = (defined, inst) {
                        selects[value.index()] = made_here(cond);
                    }
                }
            }
        }
        let mut uses = Self {
            consts,
            live: vec![false; count],
            fused,
            selects,
            folding,
        };
        let mut work = Vec::new();
        for &block in order {
            let data = &func.blocks()[block];
            for (defined, inst) in &data.insts {
                if !pure(inst) {
                    uses.need_operands(func, *defined, inst, &mut work);
                }
            }
            match data.term {
                Terminator::Branch { cond, .. } => match uses.fused[block] {
                    Some(compare) => {
                        uses.need(compare.a, &mut work);
                        if uses.branch_folded(func, compare).is_none() {
                            uses.need(compare.b, &mut work);
                        }
                    }
                    None => uses.need(cond, &mut work),
                },
                Terminator::Return { value: Some(value) } => uses.need(value, &mut work),
                Terminator::Return { value: None } | Terminator::Jump { .. } => {}
            }
        }
        while let Some(value) = work.pop() {
            match defs[value.index()] {
                Def::Inst { block, index } => {
                    let (defined, inst) = &func.blocks()[block].insts[index];
                    if pure(inst) {
                        uses.need_operands(func, *defined, inst, &mut work);
                    }
                }
                Def::Param { block, index } => {
                    for &(pred, which) in &incoming[block] {
                        let arg = func.blocks()[pred]
                            .term
                            .successors()
                            .nth(which)
                            .and_then(|(_, args)| args.get(index).copied());
                        if let Some(arg) = arg {
                            uses.need(arg, &mut work);
                        }
                    }
                }
                Def::None => {}
            }
        }
        uses
    }

    fn need(&mut self, value: Value, work: &mut Vec<Value>) {
        need(&mut self.live, value, work);
    }

    fn need_operands(
        &mut self,
        func: &Function,
        defined: Option<Value>,
        inst: &Inst,
        work: &mut Vec<Value>,
    ) {
        let Self {
            consts,
            live,
            selects,
            folding,
            ..
        } = self;
        let read = Reads {
            consts,
            selects,
            folding: *folding,
        };
        read.each(func, defined, inst, &mut |value| need(live, value, work));
    }

    /// Calls `read` with each value `inst`, defining `defined`, reads in a
    /// register or a slot: its operands not folded; for a select testing
    /// its comparison directly, that comparison's in place of its
    /// condition.
    pub(crate) fn reads(
        &self,
        func: &Function,
        defined: Option<Value>,
        inst: &Inst,
        mut read: impl FnMut(Value),
    ) {
        Reads {
            consts: &self.consts,
            selects: &self.selects,
            folding: self.folding,
        }
        .each(func, defined, inst, &mut read);
    }

    /// The comparison the select defining `value` tests directly, if it
    /// does.
    pub(crate) fn fused_select(&self, value: Value) -> Option<Compare> {
        self.selects.get(value.index()).copied().flatten()
    }

    /// Whether `value` is needed in a register or a slot.
    pub(crate) fn live(&self, value: Value) -> bool {
        self.live.get(value.index()).copied().unwrap_or(false)
    }

    /// Whether `inst`, defining `defined`, is lowered.
    pub(crate) fn kept(&self, defined: Option<Value>, inst: &Inst) -> bool {
        !pure(inst) || defined.is_some_and(|value| self.live(value))
    }

    /// The constant `value`, operand `operand` of `inst`, is, where the
    /// target folds it into the instruction.
    pub(crate) fn folded(
        &self,
        func: &Function,
        inst: &Inst,
        operand: usize,
        value: Value,
    ) -> Option<u64> {
        folded(&self.consts, self.folding, func, (inst, operand, value))
    }

    /// The comparison block `block`'s branch tests directly, if it does.
    pub(crate) fn fused(&self, block: usize) -> Option<Compare> {
        self.fused.get(block).copied().flatten()
    }

    /// The constant a fused comparison's second operand is, where the
    /// target folds it into the branch or select testing it.
    pub(crate) fn branch_folded(&self, func: &Function, compare: Compare) -> Option<u64> {
        branch_folded(&self.consts, self.folding, func, compare)
    }
}

fn need(live: &mut [bool], value: Value, work: &mut Vec<Value>) {
    if let Some(live) = live.get_mut(value.index()) {
        if !*live {
            *live = true;
            work.push(value);
        }
    }
}

fn folded(
    consts: &[Option<u64>],
    folding: Folding,
    func: &Function,
    (inst, operand, value): (&Inst, usize, Value),
) -> Option<u64> {
    let imm = consts.get(value.index()).copied().flatten()?;
    (folding.inst)(inst, operand, func.type_of(value), imm).then_some(imm)
}

fn branch_folded(
    consts: &[Option<u64>],
    folding: Folding,
    func: &Function,
    compare: Compare,
) -> Option<u64> {
    let imm = consts.get(compare.b.index()).copied().flatten()?;
    (folding.branch)(compare.cond, func.type_of(compare.b), imm).then_some(imm)
}

/// What deciding an instruction's reads needs of [`Uses`], apart from the
/// liveness it is used to find.
struct Reads<'a> {
    consts: &'a [Option<u64>],
    selects: &'a [Option<Compare>],
    folding: Folding,
}

impl Reads<'_> {
    fn each(
        &self,
        func: &Function,
        defined: Option<Value>,
        inst: &Inst,
        read: &mut impl FnMut(Value),
    ) {
        let fused = defined.and_then(|value| self.selects.get(value.index()).copied().flatten());
        if let (&Inst::Select { a, b, .. }, Some(compare)) = (inst, fused) {
            read(compare.a);
            if branch_folded(self.consts, self.folding, func, compare).is_none() {
                read(compare.b);
            }
            for (operand, value) in [(1, a), (2, b)] {
                if folded(self.consts, self.folding, func, (inst, operand, value)).is_none() {
                    read(value);
                }
            }
            return;
        }
        for (operand, value) in inst.uses().enumerate() {
            if folded(self.consts, self.folding, func, (inst, operand, value)).is_none() {
                read(value);
            }
        }
    }
}

#[cfg(test)]
#[path = "uses_tests.rs"]
mod tests;

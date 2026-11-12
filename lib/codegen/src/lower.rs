//! The lowering every backend shares: the blocks the entry reaches laid out
//! in reverse postorder, the needed values allocated, the function's
//! arguments moved from the argument registers to where they live, each kept
//! instruction handed to the backend — a call between the saving of the
//! caller-saved registers live across it and their restoring — and each
//! edge's arguments moved into its block's needed parameters as one parallel
//! move. Where both edges of a branch move values, the second's moves are
//! put after the last block, so the first still falls through to the block
//! laid out next. A backend lowers one instruction at a time and supplies
//! the moves, jumps and returns the driver asks for.

use alloc::vec::Vec;

use crate::ir::{Cond, Function, Inst, Terminator, Type, Value};
use crate::moves::{sequence, Move};
use crate::regalloc::{self, Allocation, Loc, RegClass};
use crate::uses::{Folding, Uses};
use crate::verify::reverse_postorder;
use crate::CompileError;

/// A place in the code a jump goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Label(pub(crate) usize);

/// A target as the shared lowering sees it.
pub(crate) struct Machine {
    /// The registers it allocates.
    pub(crate) regs: RegClass,
    /// The constants it encodes as immediates.
    pub(crate) folding: Folding,
}

/// What a branch tests.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Test {
    /// Whether the 32-bit value at a place is not zero.
    Value(Loc),
    /// Whether `a cond b`, `b` the immediate `imm` where the target folds it
    /// into the branch.
    Compare {
        cond: Cond,
        a: Value,
        b: Value,
        imm: Option<u64>,
    },
}

/// What lowering one instruction may read of the function.
pub(crate) struct Context<'a> {
    func: &'a Function,
    alloc: &'a Allocation,
    uses: &'a Uses,
}

impl Context<'_> {
    /// Where `value` lives.
    ///
    /// # Errors
    ///
    /// [`CompileError::Malformed`] for a value with no place, which no
    /// verified function reads where it is lowered.
    pub(crate) fn loc(&self, value: Value) -> Result<Loc, CompileError> {
        self.alloc.loc(value).ok_or(CompileError::Malformed)
    }

    /// Where the value an instruction defines lives: none where nothing
    /// needs it.
    pub(crate) fn dest(&self, defined: Option<Value>) -> Option<Loc> {
        defined.and_then(|value| self.alloc.loc(value))
    }

    /// The type of `value`.
    pub(crate) fn ty(&self, value: Value) -> Type {
        self.func.type_of(value)
    }

    /// The constant `value`, operand `operand` of `inst`, is, where the
    /// target folds it into the instruction.
    pub(crate) fn folded(&self, inst: &Inst, operand: usize, value: Value) -> Option<u64> {
        self.uses.folded(self.func, inst, operand, value)
    }

    /// The comparison the select defining `defined` tests directly, if it
    /// does.
    pub(crate) fn select_test(&self, defined: Option<Value>) -> Option<Test> {
        let compare = self.uses.fused_select(defined?)?;
        Some(Test::Compare {
            cond: compare.cond,
            a: compare.a,
            b: compare.b,
            imm: self.uses.branch_folded(self.func, compare),
        })
    }
}

/// What a backend does for the driver.
pub(crate) trait Backend {
    /// The register a parallel move breaks a cycle through.
    fn temp(&self) -> Loc;
    /// The registers the function's arguments arrive in, in order.
    fn arg_regs(&self) -> &'static [u8];
    /// A new label.
    fn label(&mut self) -> Label;
    /// The code from here is at `label`.
    fn bind(&mut self, label: Label);
    /// Save what the allocation's callee-saved registers hold and make the
    /// frame.
    ///
    /// # Errors
    ///
    /// [`CompileError::TooLarge`] for a frame the target cannot address.
    fn prologue(&mut self, alloc: &Allocation) -> Result<(), CompileError>;
    /// An argument of `ty` has arrived at `loc`, as the calling convention
    /// passes it: make it what the backend holds a value of `ty` as.
    ///
    /// # Errors
    ///
    /// Why it could not be.
    fn arrived(&mut self, loc: Loc, ty: Type) -> Result<(), CompileError> {
        let _ = (loc, ty);
        Ok(())
    }
    /// Move the whole word at `from` to `to`.
    ///
    /// # Errors
    ///
    /// Why it could not be.
    fn mov(&mut self, from: Loc, to: Loc) -> Result<(), CompileError>;
    /// Lower `inst`, defining `defined`.
    ///
    /// # Errors
    ///
    /// Why it could not be.
    fn inst(
        &mut self,
        cx: &Context<'_>,
        defined: Option<Value>,
        inst: &Inst,
    ) -> Result<(), CompileError>;
    /// Go to `label`.
    fn jump(&mut self, label: Label);
    /// Go to `label` where `test` fails.
    ///
    /// # Errors
    ///
    /// Why it could not be.
    fn jump_unless(
        &mut self,
        cx: &Context<'_>,
        test: Test,
        label: Label,
    ) -> Result<(), CompileError>;
    /// Answer `value`, of its type, undo the frame and return.
    ///
    /// # Errors
    ///
    /// Why it could not be.
    fn ret(&mut self, value: Option<(Loc, Type)>, alloc: &Allocation) -> Result<(), CompileError>;
}

/// Lower `func`, verified, for `machine` through `backend`.
///
/// # Errors
///
/// The backend's refusal, or [`CompileError::TooLarge`] for a frame of more
/// slots than are numbered.
pub(crate) fn lower(
    func: &Function,
    machine: &Machine,
    backend: &mut dyn Backend,
) -> Result<(), CompileError> {
    let order = reverse_postorder(func);
    let uses = Uses::new(func, &order, machine.folding);
    let alloc = regalloc::allocate(func, order, &uses, &machine.regs)?;
    let cx = Context {
        func,
        alloc: &alloc,
        uses: &uses,
    };
    let labels: Vec<Label> = (0..func.blocks().len()).map(|_| backend.label()).collect();
    backend.prologue(&alloc)?;
    let arguments: Vec<Move> = func
        .params()
        .iter()
        .zip(backend.arg_regs())
        .filter_map(|(&param, &reg)| {
            alloc.loc(param).map(|to| Move {
                from: Loc::Reg(reg),
                to,
            })
        })
        .collect();
    moves(backend, &arguments)?;
    for &param in func.params() {
        if let Some(loc) = alloc.loc(param) {
            backend.arrived(loc, func.type_of(param))?;
        }
    }
    let mut late: Late = Vec::new();
    // Calls laid out so far: the allocation numbers their saves so.
    let mut calls = 0;
    for (n, &block) in alloc.order.iter().enumerate() {
        let data = &func.blocks()[block];
        backend.bind(labels[block]);
        for (defined, inst) in &data.insts {
            if inst.is_call() {
                for (reg, slot) in alloc.saves(calls) {
                    backend.mov(Loc::Reg(reg), Loc::Slot(slot))?;
                }
                backend.inst(&cx, *defined, inst)?;
                for (reg, slot) in alloc.saves(calls) {
                    backend.mov(Loc::Slot(slot), Loc::Reg(reg))?;
                }
                calls += 1;
            } else if uses.kept(*defined, inst) {
                backend.inst(&cx, *defined, inst)?;
            }
        }
        let next = alloc.order.get(n + 1).copied();
        match &data.term {
            Terminator::Return { value } => {
                let value = match *value {
                    Some(value) => Some((cx.loc(value)?, func.type_of(value))),
                    None => None,
                };
                backend.ret(value, &alloc)?;
            }
            Terminator::Jump { to, args } => {
                moves(backend, &edge(&cx, to.index(), args)?)?;
                if next != Some(to.index()) {
                    backend.jump(labels[to.index()]);
                }
            }
            Terminator::Branch {
                cond,
                then,
                then_args,
                other,
                other_args,
            } => branch(
                &cx,
                backend,
                (&labels, &mut late),
                (block, next),
                (
                    *cond,
                    (then.index(), then_args),
                    (other.index(), other_args),
                ),
            )?,
        }
    }
    // The last block ends in a jump or a return, so none of these is fallen
    // into.
    for (label, edge_moves, to) in late {
        backend.bind(label);
        moves(backend, &edge_moves)?;
        backend.jump(labels[to]);
    }
    Ok(())
}

/// Edges whose moves are made after the last block: where each begins, its
/// moves, and the block it then goes to.
type Late = Vec<(Label, Vec<Move>, usize)>;

/// An edge: the block it goes to, and the arguments it gives its
/// parameters.
type Edge<'a> = (usize, &'a [Value]);

/// The branch ending `block`, `next` laid out after it, on `cond` to `then`
/// or `other`, each with its edge's arguments. The edge taken where the
/// test holds is gone through, the other jumped to; a comparison inverts, so
/// the edge whose block is laid out next can be the one fallen into.
fn branch(
    cx: &Context<'_>,
    backend: &mut dyn Backend,
    (labels, late): (&[Label], &mut Late),
    (block, next): (usize, Option<usize>),
    (cond, then, other): (Value, Edge<'_>, Edge<'_>),
) -> Result<(), CompileError> {
    let test = match cx.uses.fused(block) {
        Some(compare) => Test::Compare {
            cond: compare.cond,
            a: compare.a,
            b: compare.b,
            imm: cx.uses.branch_folded(cx.func, compare),
        },
        None => Test::Value(cx.loc(cond)?),
    };
    let falls_to_other = next == Some(other.0) && next != Some(then.0);
    let (through, away, test) = match test {
        Test::Compare { cond, a, b, imm } if falls_to_other => (
            other,
            then,
            Test::Compare {
                cond: cond.inverse(),
                a,
                b,
                imm,
            },
        ),
        _ => (then, other, test),
    };
    let away_moves = edge(cx, away.0, away.1)?;
    let to_away = if away_moves.is_empty() {
        labels[away.0]
    } else {
        backend.label()
    };
    backend.jump_unless(cx, test, to_away)?;
    moves(backend, &edge(cx, through.0, through.1)?)?;
    if next != Some(through.0) {
        backend.jump(labels[through.0]);
    }
    if !away_moves.is_empty() {
        late.push((to_away, away_moves, away.0));
    }
    Ok(())
}

/// The moves an edge to block `to` with `args` makes: to each parameter
/// something needs.
fn edge(cx: &Context<'_>, to: usize, args: &[Value]) -> Result<Vec<Move>, CompileError> {
    let params = &cx
        .func
        .blocks()
        .get(to)
        .ok_or(CompileError::Malformed)?
        .params;
    params
        .iter()
        .zip(args)
        .filter_map(|(&param, &arg)| {
            let to = cx.alloc.loc(param)?;
            Some(cx.loc(arg).map(|from| Move { from, to }))
        })
        .collect()
}

fn moves(backend: &mut dyn Backend, moves: &[Move]) -> Result<(), CompileError> {
    for step in sequence(moves, backend.temp()) {
        backend.mov(step.from, step.to)?;
    }
    Ok(())
}

//! The verifier: a function every backend may compile. Each value is defined
//! once and used only where its definition dominates; every operand has the
//! type its instruction reads; every edge gives its block's parameters their
//! types; no edge enters the entry block, whose parameters are the
//! function's arguments.

use alloc::vec;
use alloc::vec::Vec;

use crate::ir::{Block, Function, Inst, Terminator, Type, Value, MAX_ARGS};

/// Why a function is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyError {
    /// A value is defined twice, or not at all, or past the type table.
    Definition(Value),
    /// A value is used where its definition does not dominate.
    Dominance(Value),
    /// An operand or result has the wrong type.
    Type(Value),
    /// A load or store moves more than its value holds.
    Width(Value),
    /// A call or the function takes more than [`crate::ir::MAX_ARGS`].
    Arguments,
    /// An edge's arguments do not match its block's parameters.
    Edge(Block),
    /// A block is out of range, or the entry block is entered by an edge.
    Target(Block),
    /// A return does not answer what the function returns.
    Return(Block),
    /// An instruction's result and the value it defines disagree: one
    /// without the other.
    Result(Block),
}

/// Check `func`.
///
/// # Errors
///
/// The first fault found.
pub fn verify(func: &Function) -> Result<(), VerifyError> {
    let defined = definitions(func)?;
    check_types(func)?;
    let idom = dominators(func);
    for (n, block) in func.blocks.iter().enumerate() {
        if idom[n].is_none() {
            // Unreachable: never run, never compiled past its own checks.
            continue;
        }
        let def = |used: Value| {
            defined
                .get(used.index())
                .copied()
                .ok_or(VerifyError::Definition(used))
        };
        for (position, (_, inst)) in block.insts.iter().enumerate() {
            for used in inst.uses() {
                dominates(&idom, def(used)?, (n, position), used)?;
            }
        }
        let end = block.insts.len();
        for used in block.term.uses() {
            dominates(&idom, def(used)?, (n, end), used)?;
        }
    }
    Ok(())
}

/// Where each value is defined: its block, and its place in it — a block's
/// parameters before its first instruction.
fn definitions(func: &Function) -> Result<Vec<(usize, usize)>, VerifyError> {
    let mut defined: Vec<Option<(usize, usize)>> = vec![None; func.types.len()];
    let mut define = |value: Value, at: (usize, usize)| {
        let slot = defined
            .get_mut(value.index())
            .ok_or(VerifyError::Definition(value))?;
        if slot.replace(at).is_some() {
            return Err(VerifyError::Definition(value));
        }
        Ok(())
    };
    for (n, block) in func.blocks.iter().enumerate() {
        for &param in &block.params {
            define(param, (n, 0))?;
        }
        for (position, (value, _)) in block.insts.iter().enumerate() {
            if let Some(value) = *value {
                // After the instruction itself, so it reads none of its own.
                define(value, (n, position + 1))?;
            }
        }
    }
    defined
        .into_iter()
        .enumerate()
        .map(|(n, at)| {
            at.ok_or(VerifyError::Definition(Value(
                u32::try_from(n).unwrap_or(u32::MAX),
            )))
        })
        .collect()
}

/// Whether the definition at `def` dominates a use at `at` of `value`.
fn dominates(
    idom: &[Option<usize>],
    (def_block, def_at): (usize, usize),
    (use_block, use_at): (usize, usize),
    value: Value,
) -> Result<(), VerifyError> {
    if def_block == use_block {
        return if def_at <= use_at {
            Ok(())
        } else {
            Err(VerifyError::Dominance(value))
        };
    }
    let mut block = use_block;
    // Up the dominator tree; each step is to a block earlier in the order
    // the tree was built in, so the walk ends.
    for _ in 0..idom.len() {
        match idom[block] {
            Some(up) if up == block => return Err(VerifyError::Dominance(value)),
            Some(up) if up == def_block => return Ok(()),
            Some(up) => block = up,
            None => return Err(VerifyError::Dominance(value)),
        }
    }
    Err(VerifyError::Dominance(value))
}

/// Each block's immediate dominator, the entry its own; `None` for a block
/// the entry does not reach (Cooper, Harvey and Kennedy, "A Simple, Fast
/// Dominance Algorithm").
pub(crate) fn dominators(func: &Function) -> Vec<Option<usize>> {
    let order = reverse_postorder(func);
    let mut rank = vec![usize::MAX; func.blocks.len()];
    for (n, &block) in order.iter().enumerate() {
        rank[block] = n;
    }
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); func.blocks.len()];
    for &block in &order {
        for (succ, _) in func.blocks[block].term.successors() {
            preds[succ.index()].push(block);
        }
    }
    let mut idom: Vec<Option<usize>> = vec![None; func.blocks.len()];
    let Some(&entry) = order.first() else {
        return idom;
    };
    idom[entry] = Some(entry);
    let mut changed = true;
    while changed {
        changed = false;
        for &block in order.iter().skip(1) {
            let mut new = None;
            for &pred in &preds[block] {
                if idom[pred].is_none() {
                    continue;
                }
                new = Some(match new {
                    None => pred,
                    Some(other) => intersect(&idom, &rank, pred, other),
                });
            }
            if new.is_some() && idom[block] != new {
                idom[block] = new;
                changed = true;
            }
        }
    }
    idom
}

fn intersect(idom: &[Option<usize>], rank: &[usize], mut a: usize, mut b: usize) -> usize {
    while a != b {
        while rank[a] > rank[b] {
            a = idom[a].unwrap_or(a);
        }
        while rank[b] > rank[a] {
            b = idom[b].unwrap_or(b);
        }
    }
    a
}

/// The blocks the entry reaches, each after every block that dominates it.
pub(crate) fn reverse_postorder(func: &Function) -> Vec<usize> {
    let count = func.blocks.len();
    if count == 0 {
        return Vec::new();
    }
    let mut seen = vec![false; count];
    let mut post = Vec::with_capacity(count);
    // An explicit stack of (block, next successor to visit).
    let mut stack = vec![(0usize, 0usize)];
    seen[0] = true;
    while let Some(&mut (block, ref mut next)) = stack.last_mut() {
        let succ = func.blocks[block].term.successors().nth(*next);
        *next += 1;
        match succ {
            Some((to, _)) if to.index() < count && !seen[to.index()] => {
                seen[to.index()] = true;
                stack.push((to.index(), 0));
            }
            Some(_) => {}
            None => {
                post.push(block);
                stack.pop();
            }
        }
    }
    post.reverse();
    post
}

/// Every instruction's operands of the types it reads, every edge matching
/// its block, and every return the function's.
fn check_types(func: &Function) -> Result<(), VerifyError> {
    let ty = |value: Value| func.types.get(value.index()).copied();
    let want = |value: Value, expected: Type| {
        if ty(value) == Some(expected) {
            Ok(())
        } else {
            Err(VerifyError::Type(value))
        }
    };
    let entry_params = func.blocks.first().map_or(0, |entry| entry.params.len());
    if entry_params > MAX_ARGS {
        return Err(VerifyError::Arguments);
    }
    for (n, block) in func.blocks.iter().enumerate() {
        let here = Block(u32::try_from(n).unwrap_or(u32::MAX));
        for (defines, inst) in &block.insts {
            let result = match *inst {
                Inst::Const { ty, imm } => {
                    if ty.wrap(imm) != imm {
                        return Err(VerifyError::Result(here));
                    }
                    Some(ty)
                }
                Inst::Unary { a, .. } => ty(a),
                Inst::Binary { a, b, .. } => {
                    want(b, ty(a).ok_or(VerifyError::Type(a))?)?;
                    ty(a)
                }
                Inst::Icmp { a, b, .. } => {
                    want(b, ty(a).ok_or(VerifyError::Type(a))?)?;
                    Some(Type::I32)
                }
                Inst::Select { cond, a, b } => {
                    want(cond, Type::I32)?;
                    want(b, ty(a).ok_or(VerifyError::Type(a))?)?;
                    ty(a)
                }
                Inst::Extend { a, .. } => {
                    want(a, Type::I32)?;
                    Some(Type::I64)
                }
                Inst::Reduce { a } => {
                    want(a, Type::I64)?;
                    Some(Type::I32)
                }
                Inst::Load {
                    width,
                    ty: loaded,
                    base,
                    ..
                } => {
                    want(base, Type::I64)?;
                    if width.bits() > loaded.bits() {
                        return Err(VerifyError::Width(base));
                    }
                    Some(loaded)
                }
                Inst::Store {
                    width, value, base, ..
                } => {
                    want(base, Type::I64)?;
                    let held = ty(value).ok_or(VerifyError::Type(value))?;
                    if width.bits() > held.bits() {
                        return Err(VerifyError::Width(value));
                    }
                    None
                }
                Inst::Call {
                    table,
                    ref args,
                    ret,
                    ..
                } => {
                    want(table, Type::I64)?;
                    if args.len() > MAX_ARGS {
                        return Err(VerifyError::Arguments);
                    }
                    // A helper takes a word of either type, but of this
                    // function.
                    if let Some(&arg) = args.iter().find(|&&arg| ty(arg).is_none()) {
                        return Err(VerifyError::Definition(arg));
                    }
                    ret
                }
            };
            match (*defines, result) {
                (Some(value), Some(result)) => want(value, result)?,
                (None, None) => {}
                (Some(_), None) | (None, Some(_)) => return Err(VerifyError::Result(here)),
            }
        }
        check_edges(func, here, &block.term)?;
    }
    Ok(())
}

/// The block's terminator: edges to blocks there are, giving their
/// parameters' types, and returns of the function's type.
fn check_edges(func: &Function, here: Block, term: &Terminator) -> Result<(), VerifyError> {
    let ty = |value: Value| func.types.get(value.index()).copied();
    if let Terminator::Branch { cond, .. } = *term {
        if ty(cond) != Some(Type::I32) {
            return Err(VerifyError::Type(cond));
        }
    }
    if let Terminator::Return { value } = *term {
        let answered = value.map(ty);
        return match (func.ret, answered) {
            (None, None) => Ok(()),
            (Some(expected), Some(Some(got))) if expected == got => Ok(()),
            _ => Err(VerifyError::Return(here)),
        };
    }
    for (target, args) in term.successors() {
        if target.index() == 0 {
            return Err(VerifyError::Target(target));
        }
        let block = func
            .blocks
            .get(target.index())
            .ok_or(VerifyError::Target(target))?;
        if block.params.len() != args.len() {
            return Err(VerifyError::Edge(target));
        }
        for (&param, &arg) in block.params.iter().zip(args) {
            if ty(param) != ty(arg) || ty(arg).is_none() {
                return Err(VerifyError::Edge(target));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "verify_tests.rs"]
mod tests;

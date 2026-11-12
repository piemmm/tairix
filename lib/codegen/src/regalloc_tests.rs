use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::ir::{BinOp, Block, Builder, Inst, Terminator, Type};
use crate::uses::{Folding, Uses};
use crate::verify::reverse_postorder;

const CLASS: RegClass = RegClass::new(&[1, 2], &[10, 11]);
const ONE: RegClass = RegClass::new(&[1], &[]);
const KEPT_ONLY: RegClass = RegClass::new(&[], &[10]);
const CALLER: [u8; 2] = [1, 2];

const NEVER: Folding = Folding {
    inst: |_, _, _, _| false,
    branch: |_, _, _| false,
    select: false,
};

const SECOND: Folding = Folding {
    inst: |inst, operand, _, _| {
        matches!(inst, Inst::Binary { .. } | Inst::Icmp { .. }) && operand == 1
    },
    branch: |_, _, _| true,
    select: true,
};

fn allocated(func: &Function, class: &RegClass, folding: Folding) -> (Uses, Allocation) {
    let order = reverse_postorder(func);
    let uses = Uses::new(func, &order, folding);
    let alloc = allocate(func, order, &uses, class).unwrap();
    (uses, alloc)
}

/// Every value at one point of the code has a place of its own.
fn distinct(alloc: &Allocation, set: &[bool], what: &str) {
    let mut seen: Vec<(Loc, usize)> = Vec::new();
    for (v, _) in set.iter().enumerate().filter(|&(_, &live)| live) {
        let loc =
            alloc.locs[v].unwrap_or_else(|| panic!("{what}: value {v} is live with no place"));
        if let Some(&(_, other)) = seen.iter().find(|&&(at, _)| at == loc) {
            panic!("{what}: values {other} and {v} both at {loc:?}");
        }
        seen.push((loc, v));
    }
}

/// The values a kept instruction reads where they live.
fn reads(func: &Function, uses: &Uses, defined: Option<Value>, inst: &Inst) -> Vec<Value> {
    let mut read = Vec::new();
    uses.reads(func, defined, inst, |value| read.push(value));
    read
}

/// Block `b`'s live-in set from its successors', found apart from the
/// allocator by walking back over every instruction; where `check`, every
/// point on the way is held to the allocation, the first of the block's
/// calls being the `calls`th laid out.
fn walk(
    func: &Function,
    uses: &Uses,
    alloc: &Allocation,
    (b, calls): (usize, usize),
    live_in: &[Vec<bool>],
    check: bool,
) -> Vec<bool> {
    let data = &func.blocks()[b];
    let mut live = vec![false; func.value_count()];
    for (to, args) in data.term.successors() {
        for (v, &held) in live_in[to.index()].iter().enumerate() {
            live[v] |= held;
        }
        for (&param, &arg) in func.block(to).params.iter().zip(args) {
            if uses.live(param) {
                live[arg.index()] = true;
            }
        }
    }
    match data.term {
        Terminator::Branch { cond, .. } => match uses.fused(b) {
            Some(compare) => {
                live[compare.a.index()] = true;
                if uses.branch_folded(func, compare).is_none() {
                    live[compare.b.index()] = true;
                }
            }
            None => live[cond.index()] = true,
        },
        Terminator::Return { value: Some(value) } => live[value.index()] = true,
        Terminator::Return { value: None } | Terminator::Jump { .. } => {}
    }
    if check {
        distinct(alloc, &live, "at a terminator");
    }
    let mut call = calls + data.insts.iter().filter(|(_, inst)| inst.is_call()).count();
    for (defined, inst) in data.insts.iter().rev() {
        if !uses.kept(*defined, inst) {
            continue;
        }
        let read = reads(func, uses, *defined, inst);
        let result = defined.filter(|&d| uses.live(d));
        if inst.is_call() {
            call -= 1;
        }
        if check && inst.is_call() {
            let saves: Vec<(u8, u32)> = alloc.saves(call).collect();
            for (v, _) in live.iter().enumerate().filter(|&(_, &l)| l) {
                if Some(v) == result.map(Value::index) {
                    continue;
                }
                match alloc.locs[v] {
                    Some(Loc::Reg(reg)) if CALLER.contains(&reg) => assert!(
                        saves.iter().any(|&(saved, _)| saved == reg),
                        "value {v} lives across a call in {reg}, unsaved"
                    ),
                    Some(Loc::Slot(slot)) => assert!(
                        saves.iter().all(|&(_, save)| save != slot),
                        "value {v} lives across a call in slot {slot}, the save area's"
                    ),
                    _ => {}
                }
            }
            for (n, &(_, slot)) in saves.iter().enumerate() {
                assert!(
                    saves[..n].iter().all(|&(_, other)| other != slot),
                    "two registers saved to slot {slot}"
                );
            }
        }
        if check {
            let mut at = live.clone();
            for v in &read {
                at[v.index()] = true;
            }
            if let Some(d) = result {
                at[d.index()] = true;
            }
            distinct(alloc, &at, "at an instruction");
        }
        if let Some(d) = defined {
            live[d.index()] = false;
        }
        for v in read {
            live[v.index()] = true;
        }
    }
    if check {
        let mut at = live.clone();
        for &param in &data.params {
            at[param.index()] |= uses.live(param);
        }
        distinct(alloc, &at, "at a block's entry");
    }
    for &param in &data.params {
        live[param.index()] = false;
    }
    live
}

/// `alloc` against liveness found by the classic backward dataflow.
fn check(func: &Function, uses: &Uses, alloc: &Allocation) {
    let mut live_in = vec![vec![false; func.value_count()]; func.blocks().len()];
    loop {
        let mut changed = false;
        for &b in alloc.order.iter().rev() {
            let live = walk(func, uses, alloc, (b, 0), &live_in, false);
            if live != live_in[b] {
                live_in[b] = live;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut calls = 0;
    for &b in &alloc.order {
        walk(func, uses, alloc, (b, calls), &live_in, true);
        calls += func.blocks()[b]
            .insts
            .iter()
            .filter(|(_, inst)| inst.is_call())
            .count();
    }
    for (v, loc) in alloc.locs.iter().enumerate() {
        let needed = uses.live(Value(u32::try_from(v).unwrap()));
        assert_eq!(
            loc.is_some(),
            needed,
            "value {v} has a place exactly where it is needed"
        );
    }
}

#[test]
fn pressure_spills_and_a_call_keeps_what_outlives_it_safe() {
    let mut b = Builder::new(&[Type::I64, Type::I64], Some(Type::I64));
    let (x, table) = (b.params(Block::ENTRY)[0], b.params(Block::ENTRY)[1]);
    let values: Vec<_> = (0..8u64)
        .map(|n| {
            let c = b.iconst(Type::I64, n);
            b.binary(BinOp::Add, x, c)
        })
        .collect();
    let called = b.call(table, 0, &[values[0]], Some(Type::I64)).unwrap();
    let mut sum = called;
    for &v in &values {
        sum = b.binary(BinOp::Add, sum, v);
    }
    b.ret(Some(sum));
    let func = b.finish().unwrap();
    let (uses, alloc) = allocated(&func, &CLASS, NEVER);
    check(&func, &uses, &alloc);
    assert!(alloc.slots > 0, "eight values live across a call spill");
    assert_eq!(alloc.saved, [10, 11]);
}

#[test]
fn a_value_a_call_outlives_in_a_caller_saved_register_is_saved_around_it() {
    let mut b = Builder::new(&[Type::I64, Type::I64], Some(Type::I64));
    let (x, table) = (b.params(Block::ENTRY)[0], b.params(Block::ENTRY)[1]);
    let called = b.call(table, 0, &[x], Some(Type::I64)).unwrap();
    let sum = b.binary(BinOp::Add, x, called);
    b.ret(Some(sum));
    let func = b.finish().unwrap();
    let (uses, alloc) = allocated(&func, &RegClass::new(&[1, 2, 3], &[]), NEVER);
    check(&func, &uses, &alloc);
    let Some(Loc::Reg(held)) = alloc.loc(x) else {
        panic!("x is in a register: {:?}", alloc.loc(x));
    };
    assert_eq!(
        alloc.saves(0).map(|(reg, _)| reg).collect::<Vec<_>>(),
        [held]
    );
    assert_eq!(alloc.slots, 1, "one slot: the save area");
}

#[test]
fn the_value_read_least_for_its_length_is_the_one_spilled() {
    let mut b = Builder::new(&[Type::I32], Some(Type::I32));
    let hot = b.params(Block::ENTRY)[0];
    let cold = b.binary(BinOp::Add, hot, hot);
    let mut step = b.binary(BinOp::Mul, hot, cold);
    for op in [BinOp::Xor, BinOp::Sub] {
        step = b.binary(op, step, hot);
    }
    let late = b.binary(BinOp::Add, step, cold);
    let answer = b.binary(BinOp::Or, late, hot);
    b.ret(Some(answer));
    let func = b.finish().unwrap();
    let (uses, alloc) = allocated(&func, &RegClass::new(&[1, 2], &[]), NEVER);
    check(&func, &uses, &alloc);
    assert!(
        matches!(alloc.loc(hot), Some(Loc::Reg(_))),
        "the parameter every step reads keeps a register"
    );
    assert_eq!(
        alloc.loc(cold),
        Some(Loc::Slot(0)),
        "the value read twice over the whole span is the one in a slot"
    );
}

#[test]
fn a_value_nothing_reads_has_no_place() {
    let mut b = Builder::new(&[Type::I32], Some(Type::I32));
    let x = b.params(Block::ENTRY)[0];
    let c = b.iconst(Type::I32, 3);
    let dead = b.binary(BinOp::Mul, x, c);
    let kept = b.binary(BinOp::Add, x, c);
    b.ret(Some(kept));
    let func = b.finish().unwrap();
    let (uses, alloc) = allocated(&func, &CLASS, NEVER);
    check(&func, &uses, &alloc);
    assert_eq!(alloc.loc(dead), None);
    assert!(alloc.loc(c).is_some(), "read unfolded");
    let (uses, alloc) = allocated(&func, &CLASS, SECOND);
    check(&func, &uses, &alloc);
    assert_eq!(alloc.loc(c), None, "folded into its one reader");
}

#[test]
fn every_random_case_is_allocated_without_a_conflict() {
    let mut rng = tairix_fuzzseed::Prng::new(0xA11C);
    for n in 0..600 {
        let case = crate::testgen::generate(&mut || rng.next_u64()).unwrap();
        crate::verify::verify(&case.func).unwrap();
        let class = [&CLASS, &ONE, &KEPT_ONLY][n % 3];
        let folding = if n % 2 == 0 { NEVER } else { SECOND };
        let (uses, alloc) = allocated(&case.func, class, folding);
        check(&case.func, &uses, &alloc);
    }
}

/// Every slot held by intervals that never meet, in the fewest slots.
fn slots_hold(intervals: &[Interval]) {
    let mut spilled: Vec<usize> = (0..intervals.len()).collect();
    let mut locs = vec![None; intervals.len()];
    let slots = assign_slots(intervals, &mut spilled, &mut locs).unwrap();
    for (n, a) in intervals.iter().enumerate() {
        for b in &intervals[n + 1..] {
            if a.start <= b.end && b.start <= a.end {
                assert_ne!(
                    locs[a.value.index()],
                    locs[b.value.index()],
                    "{a:?} and {b:?}"
                );
            }
        }
    }
    let most = (0..=intervals.iter().map(|i| i.end).max().unwrap_or(0))
        .map(|at| {
            intervals
                .iter()
                .filter(|i| i.start <= at && at <= i.end)
                .count()
        })
        .max()
        .unwrap_or(0);
    assert_eq!(slots as usize, most, "as few slots as ever meet at once");
}

fn interval(value: u32, start: usize, end: usize) -> Interval {
    Interval {
        value: Value(value),
        start,
        end,
        reads: 1,
        crosses_call: false,
    }
}

#[test]
fn a_slot_freed_in_an_interval_is_not_given_to_it() {
    // Spilled in the order the scan gives them up: the last was evicted
    // from a register after the first two had ended and freed their slots,
    // but began while they still held them.
    let intervals = [
        interval(0, 2, 5),
        interval(1, 2, 6),
        interval(2, 7, 9),
        interval(3, 4, 20),
    ];
    let mut spilled = vec![0, 1, 2, 3];
    let mut locs = vec![None; 4];
    assert_eq!(assign_slots(&intervals, &mut spilled, &mut locs), Ok(3));
    assert_ne!(locs[3], locs[0]);
    assert_ne!(locs[3], locs[1]);
    slots_hold(&intervals);
}

#[test]
fn random_spans_share_slots_only_apart() {
    let mut rng = tairix_fuzzseed::Prng::new(0x5107);
    for _ in 0..300 {
        let count = rng.next_u64() % 24;
        let spans: Vec<Interval> = (0..count)
            .map(|v| {
                let start = usize::try_from(rng.next_u64() % 40).unwrap();
                let len = usize::try_from(rng.next_u64() % 12).unwrap();
                interval(u32::try_from(v).unwrap(), start, start + len)
            })
            .collect();
        slots_hold(&spans);
    }
}

use super::*;
use crate::ir::{BinOp, Block, Builder, Cond, Width};
use crate::verify::reverse_postorder;

const NEVER: Folding = Folding {
    inst: |_, _, _, _| false,
    branch: |_, _, _| false,
    select: false,
};

const SECOND: Folding = Folding {
    inst: |inst, operand, _, _| matches!(inst, Inst::Binary { .. }) && operand == 1,
    branch: |_, _, _| true,
    select: true,
};

fn of(func: &Function, folding: Folding) -> Uses {
    Uses::new(func, &reverse_postorder(func), folding)
}

#[test]
fn a_chain_nothing_reads_is_not_needed() {
    let mut b = Builder::new(&[Type::I64], Some(Type::I64));
    let p = b.params(Block::ENTRY)[0];
    let c = b.iconst(Type::I64, 5);
    let a = b.binary(BinOp::Add, p, c);
    let unread = b.binary(BinOp::Mul, a, a);
    let loaded = b.load(Type::I64, Width::W64, false, p, 0);
    b.store(Width::W32, c, p, 8);
    b.ret(Some(p));
    let func = b.finish().unwrap();
    let uses = of(&func, NEVER);
    assert!(!uses.live(unread) && !uses.live(a));
    assert!(!uses.live(loaded), "a load's answer may go unread");
    assert!(
        uses.kept(Some(loaded), &func.blocks()[0].insts[3].1),
        "but the load is made: it may trap"
    );
    assert!(!uses.kept(Some(a), &func.blocks()[0].insts[1].1));
    assert!(uses.live(c), "the store reads it");
    assert!(uses.live(p));
}

#[test]
fn a_constant_read_only_where_folded_is_not_needed() {
    let mut b = Builder::new(&[Type::I32], Some(Type::I32));
    let x = b.params(Block::ENTRY)[0];
    let c = b.iconst(Type::I32, 9);
    let sum = b.binary(BinOp::Add, x, c);
    b.ret(Some(sum));
    let func = b.finish().unwrap();
    assert!(!of(&func, SECOND).live(c));
    assert!(of(&func, NEVER).live(c));
    let inst = &func.blocks()[0].insts[1].1;
    assert_eq!(of(&func, SECOND).folded(&func, inst, 1, c), Some(9));
    assert_eq!(
        of(&func, SECOND).folded(&func, inst, 0, x),
        None,
        "not a constant"
    );

    let mut b = Builder::new(&[Type::I32], Some(Type::I32));
    let x = b.params(Block::ENTRY)[0];
    let c = b.iconst(Type::I32, 9);
    let sum = b.binary(BinOp::Add, c, x);
    b.ret(Some(sum));
    let func = b.finish().unwrap();
    assert!(
        of(&func, SECOND).live(c),
        "read as the first operand, which is not folded"
    );
}

#[test]
fn an_unread_parameter_does_not_need_its_arguments() {
    let mut b = Builder::new(&[Type::I32, Type::I32], Some(Type::I32));
    let (x, y) = (b.params(Block::ENTRY)[0], b.params(Block::ENTRY)[1]);
    let next = b.block(&[Type::I32, Type::I32]);
    let made = b.binary(BinOp::Xor, y, y);
    b.jump(next, &[x, made]);
    b.switch_to(next);
    let (kept, unread) = (b.params(next)[0], b.params(next)[1]);
    b.ret(Some(kept));
    let func = b.finish().unwrap();
    let uses = of(&func, NEVER);
    assert!(uses.live(kept) && uses.live(x));
    assert!(!uses.live(unread) && !uses.live(made) && !uses.live(y));
}

#[test]
fn a_value_carried_round_a_loop_is_needed_through_every_edge() {
    let mut b = Builder::new(&[Type::I32], Some(Type::I32));
    let n = b.params(Block::ENTRY)[0];
    let head = b.block(&[Type::I32, Type::I32]);
    let exit = b.block(&[Type::I32]);
    let zero = b.iconst(Type::I32, 0);
    b.jump(head, &[zero, zero]);
    b.switch_to(head);
    let (i, acc) = (b.params(head)[0], b.params(head)[1]);
    let one = b.iconst(Type::I32, 1);
    let next = b.binary(BinOp::Add, i, one);
    let more = b.binary(BinOp::Add, acc, i);
    let again = b.icmp(Cond::Ult, next, n);
    b.branch(again, (head, &[next, more]), (exit, &[more]));
    b.switch_to(exit);
    let out = b.params(exit)[0];
    b.ret(Some(out));
    let func = b.finish().unwrap();
    let uses = of(&func, SECOND);
    for value in [n, i, acc, next, more, out, zero] {
        assert!(uses.live(value), "{value:?}");
    }
    assert!(!uses.live(one), "folded");
    assert!(!uses.live(again), "the branch compares for itself");
}

#[test]
fn an_unreachable_block_needs_nothing() {
    let mut b = Builder::new(&[Type::I64], Some(Type::I32));
    let p = b.params(Block::ENTRY)[0];
    let x = b.reduce(p);
    let dead = b.block(&[]);
    b.ret(Some(x));
    b.switch_to(dead);
    let c = b.iconst(Type::I32, 1);
    b.store(Width::W8, c, p, 0);
    b.ret(Some(c));
    let func = b.finish().unwrap();
    assert!(!of(&func, NEVER).live(c));
}

#[test]
fn a_branch_on_its_own_blocks_comparison_tests_it_directly() {
    let mut b = Builder::new(&[Type::I32, Type::I32], Some(Type::I32));
    let (x, y) = (b.params(Block::ENTRY)[0], b.params(Block::ENTRY)[1]);
    let body = b.block(&[]);
    let (yes, no) = (b.block(&[]), b.block(&[Type::I32]));
    b.jump(body, &[]);
    b.switch_to(body);
    let seven = b.iconst(Type::I32, 7);
    let less = b.icmp(Cond::Slt, x, seven);
    let other = b.icmp(Cond::Eq, x, y);
    b.branch(less, (yes, &[]), (no, &[other]));
    b.switch_to(yes);
    b.ret(Some(y));
    b.switch_to(no);
    let out = b.params(no)[0];
    b.ret(Some(out));
    let func = b.finish().unwrap();
    let uses = of(&func, SECOND);
    assert_eq!(
        uses.fused(1),
        Some(Compare {
            cond: Cond::Slt,
            a: x,
            b: seven
        })
    );
    assert_eq!(uses.branch_folded(&func, uses.fused(1).unwrap()), Some(7));
    assert!(!uses.live(less), "the branch makes the comparison itself");
    assert!(!uses.live(seven), "folded into the branch");
    assert!(uses.live(other), "an edge carries it");
    let uses = of(&func, NEVER);
    assert!(uses.live(seven), "read by the branch");
    assert_eq!(uses.fused(0), None, "a jump");
}

#[test]
fn a_select_on_its_own_blocks_comparison_reads_the_comparisons_operands() {
    let mut b = Builder::new(&[Type::I32, Type::I32], Some(Type::I32));
    let (x, y) = (b.params(Block::ENTRY)[0], b.params(Block::ENTRY)[1]);
    let five = b.iconst(Type::I32, 5);
    let less = b.icmp(Cond::Ult, x, five);
    let picked = b.select(less, x, y);
    b.ret(Some(picked));
    let func = b.finish().unwrap();
    let fused = of(&func, SECOND);
    assert_eq!(
        fused.fused_select(picked),
        Some(Compare {
            cond: Cond::Ult,
            a: x,
            b: five
        })
    );
    assert!(!fused.live(less), "the select tests the comparison itself");
    assert!(
        !fused.live(five),
        "folded into the comparison the select makes"
    );
    let mut read = Vec::new();
    fused.reads(&func, Some(picked), &func.blocks()[0].insts[2].1, |value| {
        read.push(value);
    });
    assert_eq!(read, [x, x, y]);

    let apart = of(&func, NEVER);
    assert_eq!(apart.fused_select(picked), None);
    assert!(
        apart.live(less),
        "a target that cannot test it reads the value"
    );
}

#[test]
fn a_select_on_another_blocks_comparison_reads_its_value() {
    let mut b = Builder::new(&[Type::I32, Type::I32], Some(Type::I32));
    let (x, y) = (b.params(Block::ENTRY)[0], b.params(Block::ENTRY)[1]);
    let less = b.icmp(Cond::Ult, x, y);
    let next = b.block(&[]);
    b.jump(next, &[]);
    b.switch_to(next);
    let picked = b.select(less, x, y);
    b.ret(Some(picked));
    let func = b.finish().unwrap();
    let uses = of(&func, SECOND);
    assert_eq!(uses.fused_select(picked), None);
    assert!(uses.live(less));
}

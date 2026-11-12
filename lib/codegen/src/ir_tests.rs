use alloc::vec;
use alloc::vec::Vec;

use super::*;

#[test]
fn a_function_is_built_block_by_block() {
    let mut b = Builder::new(&[Type::I32, Type::I64], Some(Type::I32));
    let (x, p) = (b.params(Block::ENTRY)[0], b.params(Block::ENTRY)[1]);
    let next = b.block(&[Type::I32]);
    let one = b.iconst(Type::I32, 1);
    let sum = b.binary(BinOp::Add, x, one);
    b.store(Width::W32, sum, p, 4);
    b.jump(next, &[sum]);
    b.switch_to(next);
    let v = b.params(next)[0];
    b.ret(Some(v));
    let func = b.finish().unwrap();
    assert_eq!(func.blocks().len(), 2);
    assert_eq!(func.params(), &[x, p]);
    assert_eq!(func.type_of(sum), Type::I32);
    assert_eq!(func.block(next).params, [v]);
    assert_eq!(func.blocks()[0].insts.len(), 3);
    assert_eq!(
        func.blocks()[0].term.successors().collect::<Vec<_>>(),
        [(next, &[sum][..])]
    );
}

#[test]
fn a_constant_is_kept_to_its_type() {
    let mut b = Builder::new(&[], Some(Type::I32));
    let big = b.iconst(Type::I32, u64::MAX);
    b.ret(Some(big));
    let func = b.finish().unwrap();
    assert_eq!(
        func.blocks()[0].insts[0].1,
        Inst::Const {
            ty: Type::I32,
            imm: 0xFFFF_FFFF
        }
    );
}

#[test]
fn a_misused_builder_says_how() {
    let mut b = Builder::new(&[], None);
    b.ret(None);
    b.iconst(Type::I32, 0);
    assert_eq!(b.finish().unwrap_err(), BuildError::NoBlock);
    let mut b = Builder::new(&[], None);
    b.ret(None);
    b.switch_to(Block::ENTRY);
    b.ret(None);
    assert_eq!(b.finish().unwrap_err(), BuildError::Ended(Block::ENTRY));
    let mut b = Builder::new(&[], None);
    let other = b.block(&[]);
    b.jump(other, &[]);
    assert_eq!(b.finish().unwrap_err(), BuildError::Unended(other));
}

#[test]
fn an_instructions_operands_are_its_uses() {
    let (a, b, c) = (Value(1), Value(2), Value(3));
    let call = Inst::Call {
        table: a,
        index: 0,
        args: vec![b, c],
        ret: None,
    };
    assert_eq!(call.uses().collect::<Vec<_>>(), [a, b, c]);
    assert!(call.is_call());
    let select = Inst::Select {
        cond: a,
        a: b,
        b: c,
    };
    assert_eq!(select.uses().collect::<Vec<_>>(), [a, b, c]);
    let branch = Terminator::Branch {
        cond: a,
        then: Block(1),
        then_args: vec![b],
        other: Block(2),
        other_args: vec![c],
    };
    assert_eq!(branch.uses().collect::<Vec<_>>(), [a, b, c]);
}

#[test]
fn a_comparison_inverts_and_swaps() {
    for cond in [
        Cond::Eq,
        Cond::Ne,
        Cond::Slt,
        Cond::Sle,
        Cond::Sgt,
        Cond::Sge,
        Cond::Ult,
        Cond::Ule,
        Cond::Ugt,
        Cond::Uge,
    ] {
        assert_eq!(cond.inverse().inverse(), cond);
        assert_eq!(cond.swapped().swapped(), cond);
        for (a, b) in [(0u64, 1u64), (1, 0), (5, 5), (u64::MAX, 0)] {
            let holds = crate::interp::compare(cond, Type::I64, a, b);
            assert_eq!(
                crate::interp::compare(cond.inverse(), Type::I64, a, b),
                !holds
            );
            assert_eq!(
                crate::interp::compare(cond.swapped(), Type::I64, b, a),
                holds
            );
        }
    }
}

#[test]
fn more_values_than_32_bits_number_is_refused() {
    let mut b = Builder::new(&[], None);
    if let Ok(count) = usize::try_from(1u64 << 32) {
        assert_eq!(b.number(count), u32::MAX);
    } else {
        b.misused = Some(BuildError::TooLarge);
    }
    b.ret(None);
    assert_eq!(b.finish().unwrap_err(), BuildError::TooLarge);
}

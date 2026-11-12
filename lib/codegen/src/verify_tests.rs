use super::*;
use crate::ir::{BinOp, Builder, Inst, Type, Width};

fn refused(build: impl FnOnce(&mut Builder)) -> VerifyError {
    let mut b = Builder::new(&[Type::I32, Type::I64], Some(Type::I32));
    build(&mut b);
    verify(&b.finish().unwrap()).unwrap_err()
}

#[test]
fn a_well_formed_function_passes() {
    let mut b = Builder::new(&[Type::I32, Type::I64], Some(Type::I32));
    let (x, p) = (b.params(Block::ENTRY)[0], b.params(Block::ENTRY)[1]);
    let loop_head = b.block(&[Type::I32]);
    let exit = b.block(&[Type::I32]);
    b.jump(loop_head, &[x]);
    b.switch_to(loop_head);
    let v = b.params(loop_head)[0];
    let one = b.iconst(Type::I32, 1);
    let less = b.binary(BinOp::Sub, v, one);
    b.store(Width::W16, less, p, 0);
    b.branch(less, (loop_head, &[less]), (exit, &[v]));
    b.switch_to(exit);
    let out = b.params(exit)[0];
    b.ret(Some(out));
    assert_eq!(verify(&b.finish().unwrap()), Ok(()));
}

#[test]
fn an_operand_of_the_wrong_type_is_refused() {
    let err = refused(|b| {
        let (x, p) = (b.params(Block::ENTRY)[0], b.params(Block::ENTRY)[1]);
        let bad = b.binary(BinOp::Add, x, p);
        b.ret(Some(bad));
    });
    assert!(matches!(err, VerifyError::Type(_)), "{err:?}");
    let err = refused(|b| {
        let x = b.params(Block::ENTRY)[0];
        b.store(Width::W32, x, x, 0);
        b.ret(Some(x));
    });
    assert!(matches!(err, VerifyError::Type(_)), "{err:?}");
}

#[test]
fn a_store_wider_than_its_value_is_refused() {
    let err = refused(|b| {
        let (x, p) = (b.params(Block::ENTRY)[0], b.params(Block::ENTRY)[1]);
        b.store(Width::W64, x, p, 0);
        b.ret(Some(x));
    });
    assert!(matches!(err, VerifyError::Width(_)), "{err:?}");
}

#[test]
fn a_use_its_definition_does_not_dominate_is_refused() {
    let err = refused(|b| {
        let x = b.params(Block::ENTRY)[0];
        let left = b.block(&[]);
        let right = b.block(&[]);
        let join = b.block(&[]);
        b.branch(x, (left, &[]), (right, &[]));
        b.switch_to(left);
        let made = b.iconst(Type::I32, 3);
        b.jump(join, &[]);
        b.switch_to(right);
        b.jump(join, &[]);
        b.switch_to(join);
        b.ret(Some(made));
    });
    assert!(matches!(err, VerifyError::Dominance(_)), "{err:?}");
}

#[test]
fn an_edge_must_give_its_block_parameters_of_their_types() {
    let err = refused(|b| {
        let p = b.params(Block::ENTRY)[1];
        let next = b.block(&[Type::I32]);
        b.jump(next, &[p]);
        b.switch_to(next);
        let v = b.params(next)[0];
        b.ret(Some(v));
    });
    assert!(matches!(err, VerifyError::Edge(_)), "{err:?}");
    let err = refused(|b| {
        let x = b.params(Block::ENTRY)[0];
        let next = b.block(&[]);
        b.jump(next, &[x]);
        b.switch_to(next);
        b.ret(Some(x));
    });
    assert!(matches!(err, VerifyError::Edge(_)), "{err:?}");
}

#[test]
fn no_edge_enters_the_entry_block() {
    let err = refused(|b| {
        let x = b.params(Block::ENTRY)[0];
        let p = b.params(Block::ENTRY)[1];
        b.branch(x, (Block::ENTRY, &[x, p]), (Block::ENTRY, &[x, p]));
    });
    assert_eq!(err, VerifyError::Target(Block::ENTRY));
}

#[test]
fn a_return_must_answer_what_the_function_returns() {
    let err = refused(|b| b.ret(None));
    assert!(matches!(err, VerifyError::Return(_)), "{err:?}");
}

#[test]
fn a_call_of_too_many_arguments_is_refused() {
    let err = refused(|b| {
        let (x, p) = (b.params(Block::ENTRY)[0], b.params(Block::ENTRY)[1]);
        b.call(p, 0, &[x; MAX_ARGS + 1], None);
        b.ret(Some(x));
    });
    assert_eq!(err, VerifyError::Arguments);
    let mut b = Builder::new(&[Type::I32; MAX_ARGS + 1], None);
    b.ret(None);
    assert_eq!(verify(&b.finish().unwrap()), Err(VerifyError::Arguments));
}

#[test]
fn a_constant_out_of_its_type_is_refused() {
    let mut func = {
        let mut b = Builder::new(&[], Some(Type::I32));
        let c = b.iconst(Type::I32, 1);
        b.ret(Some(c));
        b.finish().unwrap()
    };
    func.blocks[0].insts[0].1 = Inst::Const {
        ty: Type::I32,
        imm: 1 << 40,
    };
    assert!(matches!(verify(&func), Err(VerifyError::Result(_))));
}

#[test]
fn an_unreachable_block_is_not_checked_for_dominance() {
    let mut b = Builder::new(&[Type::I32], Some(Type::I32));
    let x = b.params(Block::ENTRY)[0];
    let dead = b.block(&[]);
    b.ret(Some(x));
    b.switch_to(dead);
    b.ret(Some(x));
    assert_eq!(verify(&b.finish().unwrap()), Ok(()));
}

#[test]
fn a_value_of_another_function_is_refused_wherever_it_is_read() {
    // Past the end of this function's values: a call's argument, which no
    // type check reads, and a branch's condition.
    let mut other = Builder::new(&[Type::I64; 6], None);
    let foreign: Vec<_> = (0..20).map(|n| other.iconst(Type::I32, n)).collect();
    let far = foreign[19];
    let err = refused(|b| {
        let (x, p) = (b.params(Block::ENTRY)[0], b.params(Block::ENTRY)[1]);
        b.call(p, 0, &[far], None);
        b.ret(Some(x));
    });
    assert_eq!(err, VerifyError::Definition(far));
    let err = refused(|b| {
        let x = b.params(Block::ENTRY)[0];
        let next = b.block(&[]);
        b.branch(far, (next, &[]), (next, &[]));
        b.switch_to(next);
        b.ret(Some(x));
    });
    assert!(matches!(err, VerifyError::Type(_)), "{err:?}");
}

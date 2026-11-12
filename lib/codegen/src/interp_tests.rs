use super::*;
use crate::ir::{Block, Builder};
use crate::testgen::{TestHelpers, TestMemory, BUFFER, TABLE};

fn binary_of(op: BinOp, ty: Type, a: u64, b: u64) -> u64 {
    binary(op, ty, a, b)
}

#[test]
fn division_and_remainder_are_total() {
    for ty in [Type::I32, Type::I64] {
        assert_eq!(binary_of(BinOp::Udiv, ty, 7, 0), 0);
        assert_eq!(binary_of(BinOp::Urem, ty, 7, 0), 7);
        assert_eq!(binary_of(BinOp::Sdiv, ty, 7, 0), 0);
        assert_eq!(binary_of(BinOp::Srem, ty, 7, 0), 7);
        let min = ty.wrap(1 << (ty.bits() - 1));
        let minus_one = ty.wrap(u64::MAX);
        assert_eq!(
            binary_of(BinOp::Sdiv, ty, min, minus_one),
            min,
            "the one overflow wraps"
        );
        assert_eq!(binary_of(BinOp::Srem, ty, min, minus_one), 0);
        let minus = |n: i64| ty.wrap((-n).cast_unsigned());
        assert_eq!(binary_of(BinOp::Sdiv, ty, minus(7), 2), minus(3));
        assert_eq!(binary_of(BinOp::Srem, ty, minus(7), 2), minus(1));
    }
}

#[test]
fn shifts_and_rotates_count_modulo_the_width() {
    assert_eq!(binary_of(BinOp::Shl, Type::I32, 1, 33), 2);
    assert_eq!(binary_of(BinOp::Shl, Type::I64, 1, 65), 2);
    assert_eq!(binary_of(BinOp::Shr, Type::I32, 0x8000_0000, 31), 1);
    assert_eq!(
        binary_of(BinOp::Sar, Type::I32, 0x8000_0000, 31),
        0xFFFF_FFFF
    );
    assert_eq!(binary_of(BinOp::Rotl, Type::I32, 0x8000_0001, 1), 3);
    assert_eq!(binary_of(BinOp::Rotr, Type::I32, 3, 1), 0x8000_0001);
    assert_eq!(binary_of(BinOp::Rotr, Type::I64, 5, 64), 5);
}

#[test]
fn extension_keeps_the_low_bits_as_asked() {
    assert_eq!(unary(UnOp::Sext8, Type::I32, 0x80), 0xFFFF_FF80);
    assert_eq!(
        unary(UnOp::Sext16, Type::I64, 0x8000),
        0xFFFF_FFFF_FFFF_8000
    );
    assert_eq!(unary(UnOp::Uext8, Type::I64, 0x1FF), 0xFF);
    assert_eq!(unary(UnOp::Neg, Type::I32, 1), 0xFFFF_FFFF);
    assert_eq!(loaded(Width::W16, true, Type::I32, 0xFFFF), 0xFFFF_FFFF);
    assert_eq!(
        loaded(Width::W32, true, Type::I64, 0x8000_0000),
        0xFFFF_FFFF_8000_0000
    );
    assert_eq!(
        loaded(Width::W32, false, Type::I64, 0x8000_0000),
        0x8000_0000
    );
}

#[test]
fn a_loop_runs_and_memory_and_helpers_are_reached() {
    // Sum 1..=n into the buffer's first word, through helper 2 each time.
    let mut b = Builder::new(&[Type::I64, Type::I64, Type::I32], Some(Type::I64));
    let (base, table, n) = (
        b.params(Block::ENTRY)[0],
        b.params(Block::ENTRY)[1],
        b.params(Block::ENTRY)[2],
    );
    let head = b.block(&[Type::I32, Type::I64]);
    let exit = b.block(&[Type::I64]);
    let zero32 = b.iconst(Type::I32, 0);
    let zero64 = b.iconst(Type::I64, 0);
    b.jump(head, &[zero32, zero64]);
    b.switch_to(head);
    let (i, sum) = (b.params(head)[0], b.params(head)[1]);
    let one = b.iconst(Type::I32, 1);
    let next = b.binary(BinOp::Add, i, one);
    let wide = b.extend(false, next);
    let more = b.binary(BinOp::Add, sum, wide);
    b.store(Width::W64, more, base, 0);
    b.call(table, 2, &[more], None);
    let again = b.icmp(Cond::Ult, next, n);
    b.branch(again, (head, &[next, more]), (exit, &[more]));
    b.switch_to(exit);
    let out = b.params(exit)[0];
    b.ret(Some(out));
    let func = b.finish().unwrap();
    crate::verify::verify(&func).unwrap();
    let mut memory = TestMemory {
        buffer: vec![0; crate::testgen::BUFFER_BYTES],
    };
    let mut helpers = TestHelpers::default();
    let answer = run(
        &func,
        &[BUFFER, TABLE, 10],
        &mut memory,
        &mut helpers,
        10_000,
    )
    .unwrap();
    assert_eq!(answer, Some(55));
    assert_eq!(memory.buffer[..8], 55u64.to_le_bytes());
    assert_eq!(helpers.calls.len(), 10);
    assert_eq!(helpers.calls[9], (2, vec![55]));
    assert_eq!(
        run(&func, &[BUFFER, TABLE, 10], &mut memory, &mut helpers, 50),
        Err(Trap::Fuel)
    );
}

#[test]
fn memory_and_helpers_it_lacks_are_traps() {
    let mut b = Builder::new(&[Type::I64, Type::I64], Some(Type::I64));
    let base = b.params(Block::ENTRY)[0];
    let v = b.load(Type::I64, Width::W64, false, base, -8);
    b.ret(Some(v));
    let func = b.finish().unwrap();
    let mut memory = TestMemory {
        buffer: vec![0; crate::testgen::BUFFER_BYTES],
    };
    let mut helpers = TestHelpers::default();
    assert_eq!(
        run(&func, &[BUFFER, TABLE], &mut memory, &mut helpers, 100),
        Err(Trap::Memory(BUFFER - 8))
    );
    assert_eq!(
        run(&func, &[BUFFER], &mut memory, &mut helpers, 100),
        Err(Trap::Malformed)
    );
}

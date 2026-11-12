//! The checks every backend is held to: its code, run on its model, does
//! what the interpreter does, on random functions and on the edges each
//! operation and encoding turns on.

use alloc::vec;
use alloc::vec::Vec;

use crate::ir::{BinOp, Block, Builder, Type, Value, Width};
use crate::testgen::{agree, generate, Case, TestMemory, BUFFER, BUFFER_BYTES, TABLE};
use crate::{Isa, Target};

/// `check` on every target of `isa`: its base, and it with each set of the
/// extensions its backend lowers to.
pub(crate) fn each(isa: Isa, check: impl Fn(Target)) {
    for target in Target::ALL.into_iter().filter(|target| target.isa() == isa) {
        check(target);
    }
}

/// The seeded stream a check draws from.
pub(crate) fn seeded(seed: u64) -> impl FnMut() -> u64 {
    let mut rng = tairix_fuzzseed::Prng::new(seed);
    move || rng.next_u64()
}

fn blank() -> TestMemory {
    TestMemory {
        buffer: vec![0; BUFFER_BYTES],
    }
}

/// `count` random functions from `seed`.
pub(crate) fn random_functions_agree(target: Target, seed: u64, count: usize) {
    let mut next = seeded(seed);
    for n in 0..count {
        let case = generate(&mut next).unwrap();
        if let Err(why) = agree(target, &case, 1_000_000) {
            panic!("{target:?} case {n}: {why:?}\n{:#?}", case.func);
        }
    }
}

/// `first op second` of `ty`, `second` an argument and then a constant.
fn binary_agrees(target: Target, op: BinOp, ty: Type, (first, second): (u64, u64)) {
    for constant in [false, true] {
        let params: &[Type] = if constant {
            &[Type::I64, Type::I64, ty]
        } else {
            &[Type::I64, Type::I64, ty, ty]
        };
        let mut builder = Builder::new(params, Some(ty));
        let lhs = builder.params(Block::ENTRY)[2];
        let rhs = if constant {
            builder.iconst(ty, second)
        } else {
            builder.params(Block::ENTRY)[3]
        };
        let answer = builder.binary(op, lhs, rhs);
        builder.ret(Some(answer));
        let mut args = vec![BUFFER, TABLE, ty.wrap(first)];
        if !constant {
            args.push(ty.wrap(second));
        }
        let case = Case {
            func: builder.finish().unwrap(),
            args,
            memory: blank(),
        };
        assert_eq!(
            agree(target, &case, 100),
            Ok(()),
            "{target:?} {op:?} {ty:?} {first:#x} {second:#x} constant {constant}"
        );
    }
}

pub(crate) fn division_is_total(target: Target) {
    let edges = [
        0u64,
        1,
        2,
        7,
        u64::MAX,
        u64::MAX - 6,
        0x8000_0000,
        0x7FFF_FFFF,
        1 << 63,
        (1 << 63) - 1,
    ];
    for ty in [Type::I32, Type::I64] {
        for op in [BinOp::Udiv, BinOp::Sdiv, BinOp::Urem, BinOp::Srem] {
            for &a in &edges {
                for &b in &edges {
                    binary_agrees(target, op, ty, (a, b));
                }
            }
        }
    }
}

pub(crate) fn shifts_count_modulo_the_width(target: Target) {
    let counts = [0u64, 1, 31, 32, 33, 63, 64, 65, u64::MAX];
    for ty in [Type::I32, Type::I64] {
        for op in [BinOp::Shl, BinOp::Shr, BinOp::Sar, BinOp::Rotl, BinOp::Rotr] {
            for &a in &[1u64, 0x8000_0001, u64::MAX, 0x8000_0000_0000_0001] {
                for &b in &counts {
                    binary_agrees(target, op, ty, (a, b));
                }
            }
        }
    }
}

/// Constants each side of where a target's immediates stop: those every
/// target shares, and `own`.
pub(crate) fn immediates_on_their_edges(target: Target, own: &[u64]) {
    let shared = [
        0u64,
        1,
        0xFFF,
        0x1000,
        0x1001,
        0x7FF,
        0x800,
        0x7FFF_FFFF,
        0x8000_0000,
        0xFFFF_FFFF,
        0x1_0000_0000,
        u64::MAX,
        0xFFFF_FFFF_8000_0000,
        u64::MAX - 0xFFF,
    ];
    for ty in [Type::I32, Type::I64] {
        for op in [
            BinOp::Add,
            BinOp::Sub,
            BinOp::And,
            BinOp::Or,
            BinOp::Xor,
            BinOp::Mul,
        ] {
            for &b in shared.iter().chain(own) {
                binary_agrees(target, op, ty, (0x0123_4567_89AB_CDEF, b));
            }
        }
    }
    for &imm in shared.iter().chain(own) {
        for ty in [Type::I32, Type::I64] {
            let mut b = Builder::new(&[Type::I64, Type::I64], Some(ty));
            let c = b.iconst(ty, imm);
            b.ret(Some(c));
            let case = Case {
                func: b.finish().unwrap(),
                args: vec![BUFFER, TABLE],
                memory: blank(),
            };
            assert_eq!(
                agree(target, &case, 10),
                Ok(()),
                "{target:?} {ty:?} constant {imm:#x}"
            );
        }
    }
}

/// A function keeping `live` values across a helper call, every one read
/// after it.
fn across_a_call(live: u64) -> Case {
    let mut b = Builder::new(&[Type::I64, Type::I64, Type::I64], Some(Type::I64));
    let p = b.params(Block::ENTRY).to_vec();
    let body = b.block(&[]);
    b.jump(body, &[]);
    b.switch_to(body);
    let kept: Vec<Value> = (0..live)
        .map(|n| {
            let c = b.iconst(Type::I64, n.wrapping_mul(0x9E37_79B9));
            b.binary(BinOp::Mul, p[2], c)
        })
        .collect();
    let answer = b.call(p[1], 0, &[kept[0], p[2]], Some(Type::I64)).unwrap();
    let mut sum = answer;
    for &v in &kept {
        sum = b.binary(BinOp::Xor, sum, v);
        let one = b.iconst(Type::I64, 1);
        sum = b.binary(BinOp::Rotl, sum, one);
    }
    b.ret(Some(sum));
    Case {
        func: b.finish().unwrap(),
        args: vec![BUFFER, TABLE, 0x1234_5678],
        memory: blank(),
    }
}

/// Values outliving a call, in registers the call keeps and in slots; with
/// `deep`, so many that their slots lie beyond a load's immediate reach.
pub(crate) fn values_outlive_a_call(target: Target, deep: u64) {
    for live in [1, 5, 6, 7, 12, 40, deep] {
        let case = across_a_call(live);
        assert_eq!(
            agree(target, &case, 100_000),
            Ok(()),
            "{target:?} {live} live"
        );
    }
}

pub(crate) fn a_function_that_answers_nothing_reads_no_answer(target: Target) {
    let mut b = Builder::new(&[Type::I64, Type::I64, Type::I32], None);
    let p = b.params(Block::ENTRY).to_vec();
    b.store(Width::W32, p[2], p[0], 4);
    b.ret(None);
    let case = Case {
        func: b.finish().unwrap(),
        args: vec![BUFFER, TABLE, 0xAABB_CCDD],
        memory: blank(),
    };
    assert_eq!(agree(target, &case, 100), Ok(()));
}

/// Loads and stores at offsets each side of where a target's addressing
/// stops, from a base in the middle of the buffer.
pub(crate) fn offsets_on_their_edges(target: Target) {
    for offset in [-128i32, -120, -9, -8, -1, 0, 1, 7, 8, 9, 64, 120] {
        for width in [Width::W8, Width::W16, Width::W32, Width::W64] {
            if offset + i32::try_from(width.bytes()).unwrap() > 128 {
                continue;
            }
            let mut b = Builder::new(&[Type::I64, Type::I64, Type::I64], Some(Type::I64));
            let p = b.params(Block::ENTRY).to_vec();
            let half = b.iconst(Type::I64, 128);
            let middle = b.binary(BinOp::Add, p[0], half);
            b.store(width, p[2], middle, offset);
            let back = b.load(Type::I64, width, true, middle, offset);
            b.ret(Some(back));
            let case = Case {
                func: b.finish().unwrap(),
                args: vec![BUFFER, TABLE, 0x8899_AABB_CCDD_EEFF],
                memory: blank(),
            };
            assert_eq!(
                agree(target, &case, 100),
                Ok(()),
                "{target:?} {width:?} at {offset}"
            );
        }
    }
}

/// `compile` gives the bytes the model ran.
pub(crate) fn compile_is_the_build(
    target: Target,
    bytes: impl Fn(&crate::ir::Function) -> Vec<u8>,
) {
    let case = across_a_call(9);
    assert_eq!(
        crate::compile(&case.func, target).unwrap(),
        bytes(&case.func)
    );
}

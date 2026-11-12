use alloc::vec::Vec;

use tairix_codegen::interp::{self, Helpers, Memory, Trap};
use tairix_codegen::ir::{BinOp, Builder, Cond, Function, Inst, Type, UnOp, Width};

use super::*;

const I32: Type = Type::I32;

/// What a function that touches nothing runs on.
struct Nothing;

impl Memory for Nothing {
    fn load(&mut self, addr: u64, _: Width) -> Result<u64, Trap> {
        Err(Trap::Memory(addr))
    }
    fn store(&mut self, addr: u64, _: Width, _: u64) -> Result<(), Trap> {
        Err(Trap::Memory(addr))
    }
}

impl Helpers for Nothing {
    fn call(
        &mut self,
        _: u64,
        index: u32,
        _: &[u64],
        _: Option<Type>,
    ) -> Result<Option<u64>, Trap> {
        Err(Trap::Helper(index))
    }
}

fn emit(params: usize) -> (Emit, Vec<V>) {
    let b = Builder::new(&vec_of(I32, params), Some(I32));
    let args = b.params(Block::ENTRY).iter().map(|&v| V::R(v)).collect();
    (Emit::new(b), args)
}

fn vec_of(ty: Type, n: usize) -> Vec<Type> {
    (0..n).map(|_| ty).collect()
}

fn finish(mut e: Emit, answer: V) -> Function {
    let answer = e.val(I32, answer);
    e.b.ret(Some(answer));
    e.b.finish().unwrap()
}

fn run(func: &Function, args: &[u64]) -> u64 {
    interp::run(func, args, &mut Nothing, &mut Nothing, 10_000)
        .unwrap()
        .unwrap()
}

fn insts(func: &Function) -> Vec<Inst> {
    func.blocks()
        .iter()
        .flat_map(|b| b.insts.iter().map(|(_, inst)| inst.clone()))
        .collect()
}

#[test]
fn spread_covers_every_bit_up_to_the_highest() {
    assert_eq!(spread(0), 0);
    assert_eq!(spread(1), 1);
    assert_eq!(spread(0x1FFFE), 0x1FFFF);
    assert_eq!(spread(0x8000_0000), 0xFFFF_FFFF);
    assert_eq!(spread(u64::MAX), u64::MAX);
}

#[test]
fn a_mask_clearing_no_bit_a_value_may_have_is_no_instruction() {
    let b = Builder::new(&[Type::I64], Some(I32));
    let base = b.params(Block::ENTRY)[0];
    let mut e = Emit::new(b);
    let half = e.load(Width::W16, base, 0);
    assert_eq!(e.and(I32, half, V::K(0xFFFF)), half);
    assert_eq!(e.and(I32, half, V::K(0x1_FFFF)), half);
    let top = e.shr(I32, half, 8);
    assert_eq!(e.and(I32, top, V::K(0xFF)), top);
    assert_ne!(e.and(I32, half, V::K(0xFF)), half, "this one clears bits");
    let byte = e.load(Width::W8, base, 0);
    assert_eq!(
        e.shr(I32, byte, 8),
        V::K(0),
        "a value with no bit left is zero"
    );
}

#[test]
fn a_shift_keeps_only_the_bits_it_can_leave() {
    let (mut e, args) = emit(1);
    let x = args[0];
    let signed = e.bin(I32, BinOp::Sar, x, V::K(4));
    let kept = e.and(I32, signed, V::K(0x0FFF_FFFF));
    assert_ne!(kept, signed, "a sign may fill every bit above the shift");
    let func = finish(e, kept);
    assert_eq!(run(&func, &[0x8000_0000]), 0x0800_0000);

    let b = Builder::new(&[Type::I64], Some(I32));
    let base = b.params(Block::ENTRY)[0];
    let mut e = Emit::new(b);
    let half = e.load(Width::W16, base, 0);
    let signed = e.bin(I32, BinOp::Sar, half, V::K(4));
    assert_eq!(
        e.and(I32, signed, V::K(0x0FFF)),
        signed,
        "a sign that is clear fills nothing"
    );
    let up = e.shl(I32, half, 20);
    assert_eq!(
        e.and(I32, up, V::K(0xFFF0_0000)),
        up,
        "the bits shifted past the top are gone"
    );
    assert_ne!(e.and(I32, up, V::K(0x7FF0_0000)), up);
}

#[test]
fn a_constant_goes_second_where_the_operation_commutes() {
    let (mut e, args) = emit(1);
    let x = args[0];
    let V::R(sum) = e.add(I32, V::K(7), x) else {
        panic!("computed")
    };
    let func = finish(e, V::R(sum));
    let Some(Inst::Binary {
        op: BinOp::Add, b, ..
    }) = insts(&func)
        .into_iter()
        .find(|inst| matches!(inst, Inst::Binary { .. }))
    else {
        panic!("an addition: {func:?}");
    };
    assert!(insts(&func)
        .iter()
        .any(|inst| matches!(inst, Inst::Const { imm: 7, .. })));
    assert_ne!(V::R(b), x, "the constant is the second operand");
}

#[test]
fn identities_fold() {
    let (mut e, args) = emit(2);
    let (x, y) = (args[0], args[1]);
    assert_eq!(e.sub(I32, x, x), V::K(0));
    assert_eq!(e.xor(I32, y, y), V::K(0));
    assert_eq!(e.and(I32, x, x), x);
    assert_eq!(e.or(I32, y, y), y);
    assert_eq!(e.saturating_sub(I32, x, V::K(0)), x);
    assert_eq!(e.saturating_sub(I32, V::K(0), y), V::K(0));
    assert_eq!(e.select(I32, x, y, y), y);
    let doubled = e.bin(I32, BinOp::Mul, x, V::K(4));
    let func = finish(e, doubled);
    assert!(
        insts(&func)
            .iter()
            .any(|inst| matches!(inst, Inst::Binary { op: BinOp::Shl, .. })),
        "{func:?}"
    );
    assert_eq!(run(&func, &[3, 0]), 12);
}

#[test]
fn a_comparison_inverted_is_the_opposite_comparison() {
    let (mut e, args) = emit(2);
    let less = e.cmp(I32, Cond::Ult, args[0], args[1]);
    let not = e.not(less);
    let func = finish(e, not);
    let conds: Vec<Cond> = insts(&func)
        .iter()
        .filter_map(|inst| match *inst {
            Inst::Icmp { cond, .. } => Some(cond),
            _ => None,
        })
        .collect();
    assert_eq!(conds, [Cond::Ult, Cond::Uge]);
    assert!(!insts(&func)
        .iter()
        .any(|inst| matches!(inst, Inst::Binary { op: BinOp::Xor, .. })));
    assert_eq!(run(&func, &[1, 2]), 0);
    assert_eq!(run(&func, &[2, 1]), 1);
}

/// Random expressions over two values, built through the folding emitter,
/// answer what the operations answer applied one by one.
#[test]
fn folded_expressions_answer_what_their_operations_do() {
    let mut rng = tairix_fuzzseed::Prng::new(0xE417_0001);
    let ops = [
        BinOp::Add,
        BinOp::Sub,
        BinOp::Mul,
        BinOp::And,
        BinOp::Or,
        BinOp::Xor,
        BinOp::Shl,
        BinOp::Shr,
        BinOp::Sar,
    ];
    let unary = [
        UnOp::Uext8,
        UnOp::Uext16,
        UnOp::Sext8,
        UnOp::Sext16,
        UnOp::Neg,
        UnOp::Not,
    ];
    let constants = [
        0u64,
        1,
        2,
        4,
        8,
        15,
        16,
        0x7F,
        0xFF,
        0x3FFF,
        0xFFFF,
        0x8000,
        0x00FF_FFFF,
        0x0FFF_FFFF,
        0x7FFF_FFFF,
        0xFFFF_FFFF,
    ];
    for _ in 0..2000 {
        let inputs = [rng.next_u32(), rng.next_u32()].map(u64::from);
        let (mut emitter, args) = emit(2);
        // Each value built, beside what it must evaluate to.
        let mut built: Vec<(V, u64)> = args.into_iter().zip(inputs).collect();
        for _ in 0..12 {
            let operand = |rng: &mut tairix_fuzzseed::Prng| {
                if rng.below(4) == 0 {
                    let constant = *rng.pick(&constants);
                    (V::K(constant), constant)
                } else {
                    built[rng.below(built.len())]
                }
            };
            let (left, lhs) = operand(&mut rng);
            let (right, rhs) = operand(&mut rng);
            let next = match rng.below(6) {
                0 => {
                    let op = *rng.pick(&unary);
                    (emitter.un(I32, op, left), interp::unary(op, I32, lhs))
                }
                1 => {
                    let cond = *rng.pick(&[Cond::Eq, Cond::Ne, Cond::Ult, Cond::Sgt]);
                    let compared = emitter.cmp(I32, cond, left, right);
                    let holds = u64::from(interp::compare(cond, I32, lhs, rhs));
                    if rng.below(2) == 0 {
                        (compared, holds)
                    } else {
                        (emitter.not(compared), holds ^ 1)
                    }
                }
                2 => {
                    let (choice, chosen) = operand(&mut rng);
                    let picked = if I32.wrap(chosen) != 0 { lhs } else { rhs };
                    (emitter.select(I32, choice, left, right), picked)
                }
                3 => (
                    emitter.saturating_sub(I32, left, right),
                    I32.wrap(lhs).saturating_sub(I32.wrap(rhs)),
                ),
                _ => {
                    let op = *rng.pick(&ops);
                    (
                        emitter.bin(I32, op, left, right),
                        interp::binary(op, I32, lhs, rhs),
                    )
                }
            };
            built.push((next.0, I32.wrap(next.1)));
        }
        let (answer, expected) = *built.last().unwrap();
        let func = finish(emitter, answer);
        assert_eq!(run(&func, &inputs), expected, "{inputs:x?}\n{func:#?}");
    }
}

//! Deterministic fuzz harness for `lib/codegen`, the generator that turns
//! untrusted guest code and scripts into machine code.
//!
//! Harness invariants, checked over random functions:
//!
//! * every backend's code, run on its machine model, answers, stores and
//!   calls exactly as the IR interpreter does, on functions that keep many
//!   values live across calls, branch, loop and touch memory;
//! * any function a builder can be made to produce — ill-typed, using values
//!   of another function or of blocks that do not dominate, jumping into the
//!   entry, giving edges the wrong arguments — is refused by the verifier or
//!   compiled, never a panic; and every function the verifier admits
//!   compiles for every target.
//!
//! TAIRiX pulls in no external fuzz runner: a per-run-seeded `Prng` produces
//! the functions. A plain `cargo test` runs the [`SMOKE_ITERATIONS`] sweep
//! once from a fresh, logged seed; `cargo xtask fuzz` exports
//! `TAIRIX_FUZZ_BUDGET_SECS` to extend the loop to a wall-clock budget.

use tairix_codegen::interp;
use tairix_codegen::ir::{BinOp, Block, Builder, Cond, Type, UnOp, Value, Width};
use tairix_codegen::testgen::{agree, generate, Case, TestHelpers, TestMemory, BUFFER_BYTES};
use tairix_codegen::{compile, verify, CompileError, Target};
use tairix_fuzzseed::Prng;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 300;

#[test]
fn every_backend_agrees_with_the_interpreter() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "every_backend_agrees_with_the_interpreter",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    loop {
        let case =
            generate(&mut || rng.next_u64()).expect("the generator builds well-formed cases");
        for target in Target::ALL {
            if let Err(why) = agree(target, &case, 1_000_000) {
                panic!(
                    "{target:?} parted from the interpreter: {why:?}\n{:#?}",
                    case.func
                );
            }
        }
        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}

/// A value of `made`, or now and then, or where there are none, one of
/// another function.
fn any(rng: &mut Prng, made: &[Value], decoys: &[Value]) -> Value {
    if made.is_empty() || rng.below(8) == 0 {
        *rng.pick(decoys)
    } else {
        *rng.pick(made)
    }
}

/// A function of random instructions over values chosen without regard to
/// type, block or function.
fn hostile(rng: &mut Prng, decoys: &[Value]) -> Builder {
    let types = [Type::I32, Type::I64];
    let params: Vec<Type> = (0..rng.at_most(7)).map(|_| *rng.pick(&types)).collect();
    let ret = *rng.pick(&[None, Some(Type::I32), Some(Type::I64)]);
    let mut b = Builder::new(&params, ret);
    let mut made: Vec<Value> = b.params(Block::ENTRY).to_vec();
    let mut blocks = vec![Block::ENTRY];
    for _ in 0..rng.at_most(5) {
        let count = rng.at_most(3);
        let block = b.block(&(0..count).map(|_| *rng.pick(&types)).collect::<Vec<_>>());
        made.extend_from_slice(b.params(block));
        blocks.push(block);
    }
    for &block in &blocks {
        b.switch_to(block);
        for _ in 0..rng.at_most(12) {
            let (x, y, z) = (
                any(rng, &made, decoys),
                any(rng, &made, decoys),
                any(rng, &made, decoys),
            );
            let value = match rng.below(10) {
                0 => b.iconst(*rng.pick(&types), rng.next_u64()),
                1 => b.unary(
                    *rng.pick(&[UnOp::Neg, UnOp::Not, UnOp::Sext8, UnOp::Uext16]),
                    x,
                ),
                2 => b.binary(
                    *rng.pick(&[BinOp::Add, BinOp::Sdiv, BinOp::Rotl, BinOp::Urem]),
                    x,
                    y,
                ),
                3 => b.icmp(*rng.pick(&[Cond::Eq, Cond::Sle, Cond::Ugt]), x, y),
                4 => b.select(x, y, z),
                5 => b.extend(rng.below(2) == 0, x),
                6 => b.reduce(x),
                7 => b.load(
                    *rng.pick(&types),
                    *rng.pick(&[Width::W8, Width::W64]),
                    true,
                    x,
                    4,
                ),
                8 => {
                    b.store(*rng.pick(&[Width::W16, Width::W64]), x, y, -4);
                    continue;
                }
                _ => match b.call(x, 1, &[y, z], *rng.pick(&[None, Some(Type::I64)])) {
                    Some(value) => value,
                    None => continue,
                },
            };
            made.push(value);
        }
        let args: Vec<Value> = (0..rng.at_most(3))
            .map(|_| any(rng, &made, decoys))
            .collect();
        let to = *rng.pick(&blocks);
        match rng.below(3) {
            0 => b.jump(to, &args),
            1 => {
                let cond = any(rng, &made, decoys);
                b.branch(cond, (to, &args), (*rng.pick(&blocks), &args));
            }
            _ => {
                let value = ret.and_then(|_| made.last().copied());
                b.ret(value);
            }
        }
    }
    b
}

#[test]
fn any_built_function_is_refused_or_compiled_never_a_panic() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "any_built_function_is_refused_or_compiled_never_a_panic",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iteration: u64 = 0;
    let mut admitted = 0u64;
    loop {
        // Values of another function: past this one's, or of its types.
        let mut decoy = Builder::new(&[Type::I64; 6], None);
        let decoys: Vec<Value> = (0..20).map(|n| decoy.iconst(Type::I32, n)).collect();
        if let Ok(func) = hostile(&mut rng, &decoys).finish() {
            let verified = verify::verify(&func);
            for target in Target::ALL {
                let compiled = compile(&func, target);
                match (&verified, compiled) {
                    (Ok(()), Ok(_)) | (Err(_), Err(CompileError::Verify(_))) => {}
                    (verified, compiled) => panic!(
                        "{target:?}: verified {verified:?}, compiled {compiled:?}\n{func:#?}"
                    ),
                }
            }
            if verified.is_ok() {
                admitted += 1;
                let args: Vec<u64> = func
                    .params()
                    .iter()
                    .map(|_| rng.next_u64() & 0xFF)
                    .collect();
                let case = Case {
                    func,
                    args: args.clone(),
                    memory: TestMemory {
                        buffer: vec![0; BUFFER_BYTES],
                    },
                };
                // Run where the interpreter itself finishes: hostile code
                // may loop, or reach memory and helpers there are none of.
                let mut memory = case.memory.clone();
                let ran = interp::run(
                    &case.func,
                    &args,
                    &mut memory,
                    &mut TestHelpers::default(),
                    10_000,
                );
                if ran.is_ok() {
                    for target in Target::ALL {
                        if let Err(why) = agree(target, &case, 10_000) {
                            panic!("{target:?}: {why:?}\n{:#?}", case.func);
                        }
                    }
                }
            }
        }
        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS * 10 {
            break;
        }
    }
    assert!(admitted > 0, "some hostile function passes the verifier");
}

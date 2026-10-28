//! Deterministic fuzz harness for the SPIR-V decoder.
//!
//! A module is untrusted input — an application supplies it, and it may have
//! crossed a network — so each round mutates valid modules: bits flipped,
//! words overwritten with values near the boundaries the decoder checks, words
//! inserted, deleted and duplicated, the module cut short, and its byte order
//! reversed. The invariants are:
//!
//! 1. Nothing panics, whatever the input.
//! 2. A refusal names a word within the module.
//! 3. An accepted module re-encodes, instruction by instruction, to exactly
//!    its own bytes, and decodes from either byte order to the same module.
//! 4. Every id an accepted module defines resolves to the instruction that
//!    defines it, and every operand of every instruction has a value.
//!
//! Runs the fixed smoke sweep under plain `cargo test`; keeps drawing from the
//! same seeded stream until `TAIRIX_FUZZ_BUDGET_SECS` elapses under
//! `cargo xtask fuzz`.

use tairix_fuzzseed::Prng;
use tairix_spirv::{Arg, EncodeError, Encoder, Id, Module, Op, Version};

/// Rounds per sweep. Miri interprets every operation, so it drives a handful.
const ROUNDS_PER_SWEEP: u32 = if cfg!(miri) { 4 } else { 400 };

/// Mutants decoded per round.
const MUTANTS_PER_ROUND: u32 = if cfg!(miri) { 4 } else { 60 };

// Enumerant values the corpus uses, as the SPIR-V specification numbers them.
const SHADER: u32 = 1;
const LOGICAL: u32 = 0;
const GLSL450: u32 = 1;
const FRAGMENT: u32 = 4;
const ORIGIN_UPPER_LEFT: u32 = 7;
const LOCATION: u32 = 30;
const INPUT: u32 = 1;
const OUTPUT: u32 = 3;
const FUNCTION_STORAGE: u32 = 7;
const LOOP_DEPENDENCY_LENGTH: u32 = 8;
const FCLAMP: u32 = 43;

/// A fragment shader: interface variables, a vector type, a loop with a loop
/// control's parameter, a phi, a three-operand extended instruction, and a
/// composite construction.
fn fragment() -> Result<Vec<u8>, EncodeError> {
    let mut e = Encoder::new(Version { major: 1, minor: 3 }, 0x0001_0002)?;
    let mut ids = [None; 24];
    for slot in &mut ids {
        *slot = Some(e.next_id()?);
    }
    let id = |n: usize| ids[n].map_or(Arg::Word(0), Arg::Id);
    let [glsl, void, fn_ty, float, vec4, in_ptr, out_ptr, input, output, main, entry, zero, one, header, body, merge, latch, phi, next, clamped, colour, func_ptr, forever, boolean] =
        core::array::from_fn(id);
    e.emit(Op::CAPABILITY, &[Arg::Word(SHADER)])?;
    e.emit(Op::EXT_INST_IMPORT, &[glsl, Arg::Text("GLSL.std.450")])?;
    e.emit(Op::MEMORY_MODEL, &[Arg::Word(LOGICAL), Arg::Word(GLSL450)])?;
    e.emit(
        Op::ENTRY_POINT,
        &[Arg::Word(FRAGMENT), main, Arg::Text("frag"), input, output],
    )?;
    e.emit(Op::EXECUTION_MODE, &[main, Arg::Word(ORIGIN_UPPER_LEFT)])?;
    e.emit(Op::DECORATE, &[input, Arg::Word(LOCATION), Arg::Word(0)])?;
    e.emit(Op::DECORATE, &[output, Arg::Word(LOCATION), Arg::Word(0)])?;
    e.emit(Op::TYPE_VOID, &[void])?;
    e.emit(Op::TYPE_FUNCTION, &[fn_ty, void])?;
    e.emit(Op::TYPE_BOOL, &[boolean])?;
    e.emit(Op::TYPE_FLOAT, &[float, Arg::Word(32)])?;
    e.emit(Op::TYPE_VECTOR, &[vec4, float, Arg::Word(4)])?;
    e.emit(Op::TYPE_POINTER, &[in_ptr, Arg::Word(INPUT), float])?;
    e.emit(Op::TYPE_POINTER, &[out_ptr, Arg::Word(OUTPUT), vec4])?;
    e.emit(
        Op::TYPE_POINTER,
        &[func_ptr, Arg::Word(FUNCTION_STORAGE), float],
    )?;
    e.emit(Op::CONSTANT, &[float, zero, Arg::Word(0)])?;
    e.emit(Op::CONSTANT, &[float, one, Arg::Word(0x3f80_0000)])?;
    e.emit(Op::CONSTANT_TRUE, &[boolean, forever])?;
    e.emit(Op::VARIABLE, &[in_ptr, input, Arg::Word(INPUT)])?;
    e.emit(Op::VARIABLE, &[out_ptr, output, Arg::Word(OUTPUT)])?;
    e.emit(Op::FUNCTION, &[void, main, Arg::Word(0), fn_ty])?;
    e.emit(Op::LABEL, &[entry])?;
    e.emit(Op::BRANCH, &[header])?;
    e.emit(Op::LABEL, &[header])?;
    e.emit(Op::PHI, &[float, phi, zero, entry, next, latch])?;
    e.emit(
        Op::LOOP_MERGE,
        &[
            merge,
            latch,
            Arg::Word(LOOP_DEPENDENCY_LENGTH),
            Arg::Word(4),
        ],
    )?;
    e.emit(Op::BRANCH_CONDITIONAL, &[forever, body, merge])?;
    e.emit(Op::LABEL, &[body])?;
    e.emit(
        Op::EXT_INST,
        &[float, clamped, glsl, Arg::Word(FCLAMP), phi, zero, one],
    )?;
    e.emit(Op::BRANCH, &[latch])?;
    e.emit(Op::LABEL, &[latch])?;
    e.emit(Op::F_ADD, &[float, next, clamped, one])?;
    e.emit(Op::BRANCH, &[header])?;
    e.emit(Op::LABEL, &[merge])?;
    e.emit(Op::COMPOSITE_CONSTRUCT, &[vec4, colour, phi, phi, phi, one])?;
    e.emit(Op::STORE, &[output, colour])?;
    e.emit(Op::RETURN, &[])?;
    e.emit(Op::FUNCTION_END, &[])?;
    Ok(e.finish())
}

/// A module of every string length a word boundary changes.
fn strings() -> Result<Vec<u8>, EncodeError> {
    let mut e = Encoder::new(Version { major: 1, minor: 0 }, 0)?;
    let target: Id = e.next_id()?;
    e.emit(Op::CAPABILITY, &[Arg::Word(SHADER)])?;
    e.emit(Op::MEMORY_MODEL, &[Arg::Word(LOGICAL), Arg::Word(GLSL450)])?;
    for text in ["", "a", "ab", "abc", "abcd", "abcdefgh", "ünïcødé"] {
        e.emit(Op::NAME, &[Arg::Id(target), Arg::Text(text)])?;
    }
    e.emit(Op::TYPE_VOID, &[Arg::Id(target)])?;
    Ok(e.finish())
}

fn words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|w| u32::from_le_bytes(*w))
        .collect()
}

fn bytes_of(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// A value near one of the boundaries the decoder checks.
fn boundary(rng: &mut Prng, bound: u32) -> u32 {
    let any = rng.next_u32();
    *rng.pick(&[
        0,
        1,
        bound.wrapping_sub(1),
        bound,
        u32::MAX,
        0xffff,
        0x1_0000,
        0x0004_0000 | u32::from(Op::TYPE_INT.code()),
        0x0001_0000,
        any,
    ])
}

fn mutate(rng: &mut Prng, seed: &[u8]) -> Vec<u8> {
    let mut w = words(seed);
    let bound = w.get(3).copied().unwrap_or(1);
    for _ in 0..=rng.below(3) {
        if w.is_empty() {
            break;
        }
        let at = rng.below(w.len());
        match rng.below(7) {
            0 => w[at] ^= 1 << rng.below(32),
            1 => w[at] = boundary(rng, bound),
            2 => w.insert(at, boundary(rng, bound)),
            3 => {
                w.remove(at);
            }
            4 => {
                let copy = w[at];
                w.insert(at, copy);
            }
            5 => w.truncate(at.max(1)),
            _ => {
                let mut bytes = bytes_of(&w);
                let index = rng.below(bytes.len());
                bytes[index] = rng.next_u8();
                if rng.below(4) == 0 {
                    bytes.truncate(index);
                }
                return bytes;
            }
        }
    }
    bytes_of(&w)
}

fn swapped(bytes: &[u8]) -> Vec<u8> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[a, b, c, d]| [d, c, b, a])
        .collect()
}

/// Hold an accepted module to the third and fourth invariants.
fn check_accepted(module: &Module) {
    let mut encoder = Encoder::rewriting(module).expect("a header");
    for instruction in module.instructions() {
        encoder.copy(module, instruction).expect("copied");
        for operand in module.operands(instruction) {
            let _ = module.value(operand);
        }
        if let Some(id) = module.result(instruction) {
            assert_eq!(module.definition(id), Some(instruction), "{id:?} resolves");
        }
    }
    let rewritten = encoder.finish();
    assert_eq!(
        rewritten,
        module.bytes(),
        "an accepted module re-encodes exactly"
    );
    let other_order = Module::decode(&swapped(module.bytes())).expect("either byte order");
    assert_eq!(other_order.bytes(), module.bytes());
    assert_eq!(other_order.instructions(), module.instructions());
}

fn run_round(rng: &mut Prng, corpus: &[Vec<u8>]) {
    let seed = rng.pick(corpus);
    for _ in 0..MUTANTS_PER_ROUND {
        let mutant = mutate(rng, seed);
        let input = if rng.below(4) == 0 {
            swapped(&mutant)
        } else {
            mutant
        };
        match Module::decode(&input) {
            Ok(module) => check_accepted(&module),
            Err(refusal) => assert!(
                refusal.word as usize <= input.len() / 4,
                "{refusal:?} names a word within the module"
            ),
        }
    }
}

#[test]
fn the_decoder_is_total_over_mutated_modules() {
    let corpus = [fragment().expect("encoded"), strings().expect("encoded")];
    for module in &corpus {
        check_accepted(&Module::decode(module).expect("the corpus is valid"));
    }
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_decoder_is_total_over_mutated_modules",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..ROUNDS_PER_SWEEP {
            run_round(&mut rng, &corpus);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}

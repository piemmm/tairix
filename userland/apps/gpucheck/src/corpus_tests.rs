//! The corpus held to the validator's admitted set, run on the software
//! device, and its comparison.

use alloc::collections::BTreeSet;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_abi::time::MonotonicClock;
use tairix_spirv::assemble::{admitted_body_ops, admitted_glsl};
use tairix_spirv::{Op, Value};

use super::{compare, families, run, Held, Output, Ran};
use crate::Difference;

/// A clock that never moves, so no deadline passes while the corpus runs.
struct Still;

impl MonotonicClock for Still {
    fn now_ns(&self) -> u64 {
        0
    }
}

fn reference() -> tairix_gpu::Device {
    crate::reference(Arc::new(Still)).expect("a software device")
}

#[test]
fn the_corpus_runs_every_instruction_the_validator_admits() {
    let (mut ops, mut numbers) = (BTreeSet::new(), BTreeSet::new());
    for family in families() {
        let entry = family.build().expect("built");
        let module = entry.run.module().module();
        for instruction in module.instructions() {
            ops.insert(instruction.op().code());
            if instruction.op() == Op::EXT_INST {
                let number = module
                    .operands(instruction)
                    .get(3)
                    .map(|operand| module.value(operand));
                match number {
                    Some(Value::Word(number)) => numbers.insert(u64::from(number)),
                    Some(Value::Number(number)) => numbers.insert(number),
                    _ => false,
                };
            }
        }
    }
    let missing: Vec<&str> = admitted_body_ops()
        .filter(|op| !ops.contains(&op.code()))
        .filter_map(Op::name)
        .collect();
    assert_eq!(
        missing,
        Vec::<&str>::new(),
        "admitted instructions the corpus runs none of"
    );
    let missing_glsl: Vec<u32> = admitted_glsl()
        .filter(|&number| !numbers.contains(&u64::from(number)))
        .collect();
    assert_eq!(
        missing_glsl,
        Vec::<u32>::new(),
        "GLSL.std.450 instructions the corpus runs none of"
    );
}

#[test]
fn every_family_runs_on_the_software_device_and_agrees_with_itself() {
    let device = reference();
    let answered = run(&device).expect("the corpus runs");
    assert_eq!(answered.len(), families().count());
    for ran in &answered {
        assert!(ran.left.is_some(), "{} ran", ran.family);
        assert!(!ran.outputs.is_empty(), "{} writes", ran.family);
    }
    let again = run(&device).expect("the corpus runs again");
    assert_eq!(compare(&again, &answered), Ok(()));
    assert_eq!(
        again, answered,
        "the software device answers alike each time"
    );
}

/// What `case`'s first word left on the software device.
fn first_of(answered: &[Ran], case: &str) -> (usize, usize, u32) {
    answered
        .iter()
        .enumerate()
        .find_map(|(family, ran)| {
            let &(at, _) = ran.outputs.iter().find(|(_, output)| output.case == case)?;
            let at = usize::try_from(at).ok()?;
            Some((family, at, *ran.left.as_ref()?.get(at)?))
        })
        .expect("a case the corpus runs")
}

#[test]
fn known_answers_are_what_the_specification_gives() {
    let answered = run(&reference()).expect("the corpus runs");
    let memory_words = families()
        .find(|family| family.name == "the corpus's memory")
        .map(|family| family.build().expect("built").start.len() / 4)
        .and_then(|words| u32::try_from(words).ok())
        .expect("the memory family");
    for (case, expected) in [
        ("IAdd", 0x9e37_79b9_u32.wrapping_add(7)),
        ("UMod", 0x9e37_79b9 % 7),
        ("BitReverse", 0x9e37_79b9_u32.reverse_bits()),
        ("FMul", (1.75f32 * -0.375).to_bits()),
        ("FunctionCall", 22),
        ("Switch", 27),
        ("LoopMerge", 55),
        // The word the atomic left, written before what it answered.
        ("AtomicIAdd", 42),
        ("ControlBarrier", 4),
        ("ArrayLength", memory_words),
        ("ImageQuerySamples", 4),
        ("DPdx", 3.0f32.to_bits()),
        ("Fwidth", 8.0f32.to_bits()),
    ] {
        let (_, _, word) = first_of(&answered, case);
        assert_eq!(word, expected, "{case}");
    }
}

#[test]
fn a_word_past_what_it_is_held_to_is_the_difference_named() {
    let expected = run(&reference()).expect("the corpus runs");
    let (family, at, word) = first_of(&expected, "FDiv");
    let with = |changed: u32| {
        let mut answered = expected.clone();
        answered[family].left.as_mut().expect("ran")[at] = changed;
        answered
    };
    assert_eq!(
        compare(&with(word + 3), &expected),
        Ok(()),
        "within its three units"
    );
    assert_eq!(
        compare(&with(word + 4), &expected),
        Err(Difference::Corpus {
            family: expected[family].family,
            case: "FDiv",
            device: word + 4,
            reference: word,
        })
    );
    let mut answered = expected.clone();
    answered[family].left = None;
    assert_eq!(
        compare(&answered, &expected),
        Ok(()),
        "a device not running a family is not held to it"
    );
}

#[test]
fn no_input_rounds_in_a_direction_a_device_chooses() {
    use super::numbers::{CONVERTED, HALVES, SNORM_2X16, SNORM_4X8, UNORM_2X16, UNORM_4X8};
    for (values, scale) in [
        (&SNORM_4X8[..], 127.0),
        (&UNORM_4X8[..], 255.0),
        (&SNORM_2X16[..], 32_767.0),
        (&UNORM_2X16[..], 65_535.0),
    ] {
        for &value in values {
            let scaled = f64::from(value) * scale;
            let from_tie = (scaled.fract().abs() - 0.5).abs();
            assert!(from_tie > 1e-6, "{value} scales to a tie");
        }
    }
    for half in [HALVES & 0xffff, HALVES >> 16] {
        let exponent = half >> 10 & 0x1f;
        assert!(
            exponent != 0 && exponent != 0x1f,
            "{half:#06x} is a normal half"
        );
    }
    assert!(
        CONVERTED >> CONVERTED.trailing_zeros() < 1 << 24,
        "within a single's precision"
    );
}

#[test]
fn a_float_reduction_holds_either_zero() {
    let subgroups = families()
        .find(|family| family.name == "the corpus's subgroups")
        .expect("the subgroup family")
        .build()
        .expect("built");
    let sums: Vec<Held> = subgroups
        .outputs
        .iter()
        .filter(|(_, output)| output.case == "GroupNonUniformFAdd")
        .map(|(_, output)| output.held)
        .collect();
    assert_eq!(sums, [Held::Ulps(0); 3]);
    assert!(Held::Ulps(0).holds(1 << 31, 0), "minus zero is zero");
    assert!(!Held::Ulps(0).holds(1, 0));
}

#[test]
fn each_hold_is_kept_to_its_bound() {
    assert!(Held::Exact.holds(7, 7));
    assert!(!Held::Exact.holds(7, 8));
    let one = 1.0f32.to_bits();
    assert!(Held::Ulps(2).holds(one + 2, one));
    assert!(!Held::Ulps(2).holds(one + 3, one));
    // Neighbouring singles across zero are neighbours.
    let (least, negative_least) = (1, 1 | 1 << 31);
    assert!(Held::Ulps(2).holds(least, negative_least));
    assert!(!Held::Ulps(1).holds(least, negative_least));
    let within = |a: f32, b: f32| Held::Within(10).holds(a.to_bits(), b.to_bits());
    assert!(within(0.5, 0.5 + 1.0 / 1024.0));
    assert!(!within(0.5, 0.5 + 1.0 / 512.0));
    assert!(!within(f32::NAN, 0.5), "a NaN is held to nothing");
    let output = Output {
        case: "Sin",
        held: Held::Within(11),
    };
    assert_eq!(output.held, Held::Within(11));
}

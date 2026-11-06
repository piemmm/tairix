use alloc::collections::BTreeSet;
use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use super::{Family, Halted, Op, Work};
use crate::canvas::OutOfMemory;
use crate::effect::{Effect, Noise};
use crate::tone::{ChannelMixer, Levels};

#[test]
fn each_family_offers_its_own_operations_once() {
    assert_eq!(Op::ADJUSTMENTS.len(), 11);
    assert_eq!(Op::EFFECTS.len(), 7);
    for family in Family::ALL {
        let ops = family.ops();
        assert_eq!(family.first(), ops[0]);
        for (place, op) in ops.iter().enumerate() {
            assert_eq!(op.family(), family, "{op:?}");
            assert_eq!(op.place(), place, "{op:?}");
            assert_eq!(op.needs_colour(), family == Family::Effects, "{op:?}");
            for (other, rival) in ops.iter().enumerate() {
                assert_eq!(op.same_kind(rival), other == place, "{op:?}, {rival:?}");
                assert_eq!(op.label() == rival.label(), other == place, "{op:?}");
            }
        }
    }
}

#[test]
fn every_setting_starts_and_is_held_within_its_bounds() {
    for op in Op::ADJUSTMENTS.into_iter().chain(Op::EFFECTS) {
        let parameters = op.parameters();
        for (index, parameter) in parameters.iter().enumerate() {
            let value = op.value(index);
            assert!(
                (parameter.least..=parameter.most).contains(&value),
                "{op:?} starts in bounds"
            );
            assert_eq!(parameter.value_of(parameter.permille_of(value)), value);
            let mut set = op;
            set.set(index, i32::MAX);
            assert_eq!(set.value(index), parameter.most, "{op:?} held to its most");
            set.set(index, i32::MIN);
            assert_eq!(
                set.value(index),
                parameter.least,
                "{op:?} held to its least"
            );
        }
        let mut untouched = op;
        untouched.set(parameters.len(), 7);
        assert_eq!(untouched, op, "{op:?} has no such number");
    }
}

#[test]
fn an_operation_set_to_change_nothing_says_so() {
    let mut quiet = Noise::START;
    quiet.amount = 0;
    let unchanging = [
        Op::BrightnessContrast {
            brightness: 0,
            contrast: 0,
        },
        Op::Levels(Levels::IDENTITY),
        Op::ChannelMixer(ChannelMixer::IDENTITY),
        Op::GaussianBlur { radius: 0 },
        Op::UnsharpMask {
            amount: 0,
            radius: 3,
            threshold: 0,
        },
        Op::Median { radius: 0 },
        Op::AddNoise(quiet),
        Op::Pixelate { cell: 1 },
    ];
    for op in unchanging {
        assert!(op.is_identity(), "{op:?}");
    }
    let changing = [
        Op::Desaturate,
        Op::Invert,
        Op::FindEdges,
        Op::Posterise { levels: 64 },
        Op::Threshold { level: 0 },
        Op::GaussianBlur { radius: 1 },
        Op::AddNoise(Noise::START),
    ];
    for op in changing {
        assert!(!op.is_identity(), "{op:?}");
    }
}

#[test]
fn an_adjustment_maps_a_palette_and_an_effect_takes_none() {
    let palette = [[0, 0, 0, 255], [255, 255, 255, 128], [10, 200, 30, 255]];
    let inverted = vec![[255, 255, 255, 255], [0, 0, 0, 128], [245, 55, 225, 255]];
    assert_eq!(Op::Invert.mapped_palette(&palette), Some(inverted));
    assert_eq!(
        Op::GaussianBlur { radius: 2 }.mapped_palette(&palette),
        None
    );
    assert!(matches!(Op::Invert.work(), Work::Tone(_)));
    assert!(matches!(
        Op::Median { radius: 99 }.work(),
        Work::Effect(Effect::Median(50))
    ));
}

#[test]
fn a_halt_says_why_it_stopped() {
    assert_eq!(Halted::from(OutOfMemory), Halted::OutOfMemory);
    let reasons = [Halted::Abandoned, Halted::OutOfMemory, Halted::NeedsColour]
        .map(|halted| format!("{halted}"));
    assert!(reasons.iter().all(|reason| !reason.is_empty()));
    assert!(reasons[0] != reasons[1] && reasons[1] != reasons[2] && reasons[0] != reasons[2]);
}

#[test]
fn the_menu_lists_every_operation_once_each_by_a_function_of_its_own() {
    let every: Vec<Op> = Op::every().collect();
    assert_eq!(every.len(), Op::COUNT);
    assert_eq!(every[..Op::ADJUSTMENTS.len()], Op::ADJUSTMENTS);
    assert_eq!(every[Op::ADJUSTMENTS.len()..], Op::EFFECTS);
    let symbols: BTreeSet<_> = every.iter().map(Op::symbol).collect();
    assert_eq!(symbols.len(), Op::COUNT, "no two share a function");
}

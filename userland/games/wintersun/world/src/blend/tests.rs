use alloc::vec::Vec;

use super::{Blend, Kind, BLEND_SLOTS, WEIGHT_TOTAL};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Letter {
    A,
    B,
    C,
    D,
    E,
    F,
}

impl Kind for Letter {
    const ALL: &'static [Self] = &[Self::A, Self::B, Self::C, Self::D, Self::E, Self::F];

    fn id(self) -> u8 {
        self as u8
    }
}

#[test]
fn a_solid_blend_is_normalised_and_labelled() {
    for &letter in Letter::ALL {
        let blend = Blend::solid(letter);
        assert_eq!(blend.total(), WEIGHT_TOTAL);
        assert_eq!(blend.dominant(), letter);
        assert_eq!(blend.slots().count(), 1);
        assert_eq!(blend.weight_of(letter), 255);
    }
}

#[test]
fn blends_that_weigh_the_same_kinds_alike_are_equal() {
    // Whatever answered an unclaimed cell, a blend of one kind is that kind.
    let mut raw = [0.0_f64; 6];
    raw[2] = 3.0;
    assert_eq!(Blend::normalise(&raw, Letter::F), Blend::solid(Letter::C));
    raw[4] = 1.0;
    assert_eq!(
        Blend::normalise(&raw, Letter::A),
        Blend::normalise(&raw, Letter::F)
    );
}

#[test]
fn every_raw_vector_normalises_to_the_total_heaviest_first() {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    for _ in 0..2_000 {
        let mut raw = [0.0_f64; 6];
        for weight in &mut raw {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            // A third of the entries zero, so blends of every width occur.
            let draw = u32::try_from((state >> 40) % 3000).expect("below 3000");
            *weight = if draw < 1000 {
                0.0
            } else {
                f64::from(draw) / 997.0
            };
        }
        let blend = Blend::<Letter>::normalise(&raw, Letter::A);
        assert_eq!(blend.total(), WEIGHT_TOTAL, "{raw:?}");
        let weights = blend.weights();
        for slot in 1..BLEND_SLOTS {
            assert!(weights[slot - 1] >= weights[slot], "{raw:?}");
        }
        // A vector nothing claimed is the bare kind, tested on its own; any
        // other gives weight only to what claimed it.
        if raw.iter().any(|&w| w > 0.0) {
            for (kind, weight) in blend.slots() {
                assert!(
                    raw[kind.id() as usize] > 0.0,
                    "a zero entry was given weight: {raw:?} -> {blend:?}"
                );
                assert!(weight > 0);
            }
        }
    }
}

#[test]
fn only_the_four_heaviest_survive() {
    let raw = [1.0, 6.0, 2.0, 5.0, 4.0, 3.0];
    let blend = Blend::<Letter>::normalise(&raw, Letter::A);
    let kinds: Vec<Letter> = blend.slots().map(|(kind, _)| kind).collect();
    assert_eq!(kinds, [Letter::B, Letter::D, Letter::E, Letter::F]);
}

#[test]
fn a_tie_resolves_to_the_lower_identifier() {
    let raw = [0.0, 1.0, 0.0, 1.0, 0.0, 0.0];
    let blend = Blend::<Letter>::normalise(&raw, Letter::A);
    assert_eq!(blend.dominant(), Letter::B);
    assert_eq!(blend.weight_of(Letter::B), 128);
    assert_eq!(blend.weight_of(Letter::D), 127);
}

#[test]
fn a_cell_nothing_claimed_is_the_bare_kind() {
    let blend = Blend::<Letter>::normalise(&[0.0; 6], Letter::E);
    assert_eq!(blend, Blend::solid(Letter::E));
    let negative = Blend::<Letter>::normalise(&[-1.0; 6], Letter::C);
    assert_eq!(negative, Blend::solid(Letter::C));
}

#[test]
fn normalisation_is_a_pure_function() {
    let raw = [0.3, 0.0, 0.7, 0.1, 0.0, 0.2];
    assert_eq!(
        Blend::<Letter>::normalise(&raw, Letter::A),
        Blend::<Letter>::normalise(&raw, Letter::A)
    );
}

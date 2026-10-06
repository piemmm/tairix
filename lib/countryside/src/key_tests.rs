use super::*;

const KEY: Key = Key::new(0x5eed);

#[test]
fn a_draw_is_its_key_stage_and_place_and_owes_nothing_to_order() {
    let mut first = KEY.draws(Stage::Cut, (3, -7));
    let mut again = KEY.draws(Stage::Cut, (3, -7));
    let words: [u64; 4] = core::array::from_fn(|_| first.word());
    assert_eq!(words, core::array::from_fn(|_| again.word()));
    assert_ne!(words[0], KEY.draws(Stage::Gate, (3, -7)).word(), "another purpose");
    assert_ne!(words[0], KEY.draws(Stage::Cut, (4, -7)).word(), "another place");
    assert_ne!(words[0], Key::new(1).draws(Stage::Cut, (3, -7)).word(), "another key");
    assert_ne!(words[0], words[1]);
}

#[test]
fn draws_keep_to_their_ranges() {
    for place in 0..2000 {
        let mut draws = KEY.draws(Stage::Use, (place, 0));
        let unit = draws.unit();
        assert!((0.0..1.0).contains(&unit));
        let ranged = draws.range(-3.0, 5.0);
        assert!((-3.0..5.0).contains(&ranged));
        assert!(draws.below(7) < 7);
        assert_eq!(draws.below(0), 0);
    }
}

#[test]
fn a_pick_is_as_likely_as_its_weight_and_never_a_weightless_one() {
    let weights = [('a', 1.0), ('b', 0.0), ('c', 3.0), ('d', -2.0)];
    let mut counts = [0u32; 4];
    for place in 0..20_000 {
        match KEY.draws(Stage::Use, (place, 1)).pick(&weights) {
            Some('a') => counts[0] += 1,
            Some('c') => counts[2] += 1,
            other => panic!("picked {other:?}"),
        }
    }
    let share = f64::from(counts[0]) / 20_000.0;
    assert!((share - 0.25).abs() < 0.015, "{share}");
    assert_eq!(KEY.draws(Stage::Use, (0, 2)).pick(&[('a', 0.0), ('b', -1.0)]), None);
}

#[test]
fn an_identity_hashed_twice_alike_draws_alike() {
    let word = |value: u32| {
        let mut hasher = KEY.hasher(Stage::Boundary);
        hasher.write_u32(value);
        hasher.finish()
    };
    assert_eq!(word(9), word(9));
    assert_ne!(word(9), word(10));
    assert_eq!(KEY.draws_for(Stage::Gate, word(9)).word(), KEY.draws_for(Stage::Gate, word(9)).word());
}

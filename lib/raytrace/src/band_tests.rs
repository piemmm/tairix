use super::*;
use tairix_parallel::{Reversed, Threaded, SERIAL};

#[test]
fn every_band_is_filled_once_with_its_own_number() {
    let runners: [&dyn JobRunner; 3] = [&SERIAL, &Reversed::new(4), &Threaded::new(4)];
    for runner in runners {
        let mut values = [0usize; 23];
        for_each(runner, &mut values, (7, 5), &|number, band| {
            for value in band {
                *value += number;
            }
        });
        for (index, value) in values.iter().enumerate() {
            assert_eq!(*value, 7 + index / 5, "{index}");
        }
    }
}

#[test]
fn nothing_to_fill_visits_nothing_and_a_zero_width_band_is_one_wide() {
    for_each(&SERIAL, &mut [0u8; 0], (0, 4), &|_, _| {
        unreachable!("no bands")
    });
    let mut values = [0usize; 3];
    for_each(&Reversed::new(2), &mut values, (1, 0), &|number, band| {
        assert_eq!(band.len(), 1);
        band[0] = number;
    });
    assert_eq!(values, [1, 2, 3]);
}

#[test]
fn a_fold_joins_every_bands_answer_in_band_order() {
    let runners: [&dyn JobRunner; 3] = [&SERIAL, &Reversed::new(4), &Threaded::new(4)];
    for runner in runners {
        let mut values = [1u64; 23];
        // A join that does not commute shows any answer taken out of order.
        let joined = fold(
            runner,
            &mut values,
            (3, 5),
            7u64,
            &|number, band| {
                for value in band.iter_mut() {
                    *value = 2;
                }
                (number as u64) * 100 + band.len() as u64
            },
            |joined, answer| joined.wrapping_mul(1_000_003).wrapping_add(answer),
        );
        let expected = [305, 405, 505, 605, 703]
            .into_iter()
            .fold(7u64, |joined, answer| {
                joined.wrapping_mul(1_000_003).wrapping_add(answer)
            });
        assert_eq!(joined, expected);
        assert!(values.iter().all(|&value| value == 2));
    }
}
